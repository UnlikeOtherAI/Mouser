//! Daemon file-transfer glue over the bulk plane.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use mouser_core::{DeviceId, DeviceIdentity};
use mouser_files::{FileSource, Outbound, Receiver, ReceiverConfig, Sender, SinkError};
use mouser_net::{BulkConnection, BulkEndpoint, PinPolicy, TransferStream};
use mouser_protocol::{
    from_cbor, to_cbor, FileAccept, FileAck, FileChunk, FileDone, FileOffer, FileReject,
    TYPE_FILE_ACCEPT, TYPE_FILE_ACK, TYPE_FILE_CHUNK, TYPE_FILE_DONE, TYPE_FILE_OFFER,
    TYPE_FILE_REJECT,
};
use tokio::sync::watch;

use crate::daemon_store::DaemonStore;
use crate::discovery::{self, PeerRegistry};

/// Temporary binding id while the interactive Hello/SAS session id is not wired yet.
pub(crate) const BULK_SESSION_ID: u64 = 0;

const RECV_RETRY_DELAY: Duration = Duration::from_secs(1);
const SEND_RECV_TIMEOUT: Duration = Duration::from_secs(30);
const HASH_BUF: usize = 64 * 1024;

static NEXT_TRANSFER_ID: AtomicU64 = AtomicU64::new(1);

/// Per-user quarantine directory for received files.
pub(crate) fn quarantine_dir(store: &DaemonStore) -> PathBuf {
    store.dir().join("quarantine")
}

/// Accept bound bulk connections and serve file-transfer streams for the active peer.
pub(crate) async fn run_bulk_acceptor(
    endpoint: Arc<BulkEndpoint>,
    active_peer: watch::Receiver<Option<DeviceId>>,
    quarantine: PathBuf,
) {
    loop {
        match endpoint.accept_bulk(BULK_SESSION_ID).await {
            Ok(conn) => {
                let expected = *active_peer.borrow();
                if expected.is_none_or(|peer| conn.peer_device_id() != Some(peer)) {
                    eprintln!("mouserd: rejected bulk connection from non-active peer");
                    conn.close();
                    continue;
                }
                let dir = quarantine.clone();
                tokio::spawn(async move {
                    if let Err(e) = serve_bulk_connection(conn, dir).await {
                        eprintln!("mouserd: bulk file receiver stopped: {e}");
                    }
                });
            }
            Err(e) => {
                eprintln!("mouserd: bulk accept skipped: {e}");
                tokio::time::sleep(RECV_RETRY_DELAY).await;
            }
        }
    }
}

#[cfg(unix)]
async fn serve_bulk_connection(conn: BulkConnection, quarantine: PathBuf) -> Result<(), String> {
    std::fs::create_dir_all(&quarantine)
        .map_err(|e| format!("create quarantine {}: {e}", quarantine.display()))?;
    loop {
        match conn.accept_transfer().await {
            Ok(stream) => {
                let dir = quarantine.clone();
                tokio::spawn(async move {
                    if let Err(e) = run_receiver_stream(stream, dir).await {
                        eprintln!("mouserd: file transfer failed: {e}");
                    }
                });
            }
            Err(e) => return Err(e.to_string()),
        }
    }
}

#[cfg(not(unix))]
async fn serve_bulk_connection(conn: BulkConnection, _quarantine: PathBuf) -> Result<(), String> {
    conn.close();
    Err("file receive is not available on this platform yet".to_string())
}

/// Programmatic daemon send API: send local files to a discovered peer's bulk port.
#[cfg(unix)]
pub async fn send_paths_to_peer(
    endpoint: Arc<BulkEndpoint>,
    identity: Arc<DeviceIdentity>,
    registry: PeerRegistry,
    peer: DeviceId,
    paths: Vec<PathBuf>,
) -> Result<(), String> {
    let advert = registry.find(&peer).ok_or_else(|| {
        format!(
            "peer {} is not in the live discovery registry",
            crate::daemon_store::format_device_id(&peer)
        )
    })?;
    let addr = discovery::peer_bulk_socket_addr(&advert).ok_or_else(|| {
        format!(
            "peer {} did not advertise a dialable bulk endpoint",
            advert.instance_name()
        )
    })?;
    send_paths_to_addr(&endpoint, identity.as_ref(), peer, addr, paths).await
}

#[cfg(not(unix))]
pub async fn send_paths_to_peer(
    _endpoint: Arc<BulkEndpoint>,
    _identity: Arc<DeviceIdentity>,
    _registry: PeerRegistry,
    _peer: DeviceId,
    _paths: Vec<PathBuf>,
) -> Result<(), String> {
    Err("file send is not available on this platform yet".to_string())
}

#[cfg(unix)]
async fn send_paths_to_addr(
    endpoint: &BulkEndpoint,
    identity: &DeviceIdentity,
    peer: DeviceId,
    addr: SocketAddr,
    paths: Vec<PathBuf>,
) -> Result<(), String> {
    let transfer_id = next_transfer_id();
    let files = prepare_sources(paths)?;
    let conn = endpoint
        .connect_bulk(identity, addr, PinPolicy::Pinned(peer), BULK_SESSION_ID)
        .await
        .map_err(|e| e.to_string())?;
    let mut stream = conn
        .open_transfer(transfer_id)
        .await
        .map_err(|e| e.to_string())?;
    let sender = Sender::new_with_hashes(transfer_id, files).map_err(|e| e.to_string())?;
    run_sender_stream(sender, &mut stream).await?;
    let _ = stream.finish();
    conn.close();
    Ok(())
}

fn next_transfer_id() -> u64 {
    let id = NEXT_TRANSFER_ID.fetch_add(1, Ordering::Relaxed);
    if id == 0 {
        NEXT_TRANSFER_ID.fetch_add(1, Ordering::Relaxed)
    } else {
        id
    }
}

async fn run_receiver_stream(
    mut stream: TransferStream,
    quarantine: PathBuf,
) -> Result<(), String> {
    let (ty, payload) = stream.recv_msg().await.map_err(|e| e.to_string())?;
    if ty != TYPE_FILE_OFFER {
        return Err(format!("expected FileOffer, got {ty:#06x}"));
    }
    let offer: FileOffer = from_cbor(&payload).map_err(|e| e.to_string())?;
    let transfer_id = offer.transfer_id;
    let config = ReceiverConfig::new(quarantine);
    let (mut receiver, out) =
        Receiver::accept_offer(&offer, config, |_i, path| open_receive_sink(path))
            .map_err(|e| e.to_string())?;
    send_outbound_batch(&mut stream, out).await?;

    while !receiver.is_terminal() {
        let (ty, payload) = stream.recv_msg().await.map_err(|e| e.to_string())?;
        if ty != TYPE_FILE_CHUNK {
            return Err(format!("expected FileChunk, got {ty:#06x}"));
        }
        let chunk: FileChunk = from_cbor(&payload).map_err(|e| e.to_string())?;
        match receiver.on_chunk(&chunk) {
            Ok(out) => send_outbound_batch(&mut stream, out).await?,
            Err(e) => {
                let done = FileDone {
                    transfer_id,
                    ok: false,
                };
                send_message(&mut stream, TYPE_FILE_DONE, &done).await?;
                return Err(e.to_string());
            }
        }
    }
    let _ = stream.finish();
    Ok(())
}

#[cfg(unix)]
fn open_receive_sink(path: &Path) -> Result<mouser_files::FsSink, SinkError> {
    mouser_files::FsSink::open(path)
}

#[cfg(not(unix))]
fn open_receive_sink(_path: &Path) -> Result<mouser_files::MemSink, SinkError> {
    Err(SinkError(
        "file receive is not available on this platform yet".to_string(),
    ))
}

async fn send_outbound_batch(
    stream: &mut TransferStream,
    out: Vec<Outbound>,
) -> Result<(), String> {
    for msg in out {
        match msg {
            Outbound::Accept(value) => send_message(stream, TYPE_FILE_ACCEPT, &value).await?,
            Outbound::Reject(value) => send_message(stream, TYPE_FILE_REJECT, &value).await?,
            Outbound::Ack(value) => send_message(stream, TYPE_FILE_ACK, &value).await?,
            Outbound::Done(value) => send_message(stream, TYPE_FILE_DONE, &value).await?,
        }
    }
    Ok(())
}

async fn send_message<T: serde::Serialize>(
    stream: &mut TransferStream,
    ty: u16,
    value: &T,
) -> Result<(), String> {
    let payload = to_cbor(value).map_err(|e| e.to_string())?;
    stream
        .send_msg(ty, &payload)
        .await
        .map_err(|e| e.to_string())
}

#[cfg(unix)]
async fn run_sender_stream(
    mut sender: Sender<DiskSource>,
    stream: &mut TransferStream,
) -> Result<(), String> {
    send_message(stream, TYPE_FILE_OFFER, &sender.offer()).await?;
    let (ty, payload) = recv_send_response(stream).await?;
    match ty {
        TYPE_FILE_ACCEPT => {
            let accept: FileAccept = from_cbor(&payload).map_err(|e| e.to_string())?;
            sender.on_accept(&accept).map_err(|e| e.to_string())?;
        }
        TYPE_FILE_REJECT => {
            let reject: FileReject = from_cbor(&payload).map_err(|e| e.to_string())?;
            return Err(format!("receiver rejected transfer: {}", reject.reason));
        }
        other => return Err(format!("expected FileAccept, got {other:#06x}")),
    }

    let mut terminal_done = None;
    loop {
        while let Some(chunk) = sender.poll_chunk().map_err(|e| e.to_string())? {
            send_message(stream, TYPE_FILE_CHUNK, &chunk).await?;
        }
        if sender.is_complete() || sender.is_aborted() {
            break;
        }
        let (ty, payload) = recv_send_response(stream).await?;
        match ty {
            TYPE_FILE_ACK => {
                let ack: FileAck = from_cbor(&payload).map_err(|e| e.to_string())?;
                sender.on_ack(&ack).map_err(|e| e.to_string())?;
            }
            TYPE_FILE_DONE => {
                let done: FileDone = from_cbor(&payload).map_err(|e| e.to_string())?;
                sender.on_done(&done).map_err(|e| e.to_string())?;
                terminal_done = Some(done);
                break;
            }
            other => return Err(format!("sender got unexpected type {other:#06x}")),
        }
    }

    let done = match terminal_done {
        Some(done) => done,
        None => {
            let (ty, payload) = recv_send_response(stream).await?;
            if ty != TYPE_FILE_DONE {
                return Err(format!("expected FileDone, got {ty:#06x}"));
            }
            let done: FileDone = from_cbor(&payload).map_err(|e| e.to_string())?;
            sender.on_done(&done).map_err(|e| e.to_string())?;
            done
        }
    };
    if sender.is_aborted() || !done.ok {
        return Err("receiver reported file transfer failure".to_string());
    }
    Ok(())
}

async fn recv_send_response(stream: &mut TransferStream) -> Result<(u16, Vec<u8>), String> {
    tokio::time::timeout(SEND_RECV_TIMEOUT, stream.recv_msg())
        .await
        .map_err(|_| "timed out waiting for file-transfer response".to_string())?
        .map_err(|e| e.to_string())
}

#[cfg(unix)]
struct DiskSource {
    file: std::fs::File,
    len: u64,
}

#[cfg(unix)]
impl FileSource for DiskSource {
    fn len(&self) -> u64 {
        self.len
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<usize, SinkError> {
        use std::os::unix::fs::FileExt;

        self.file
            .read_at(buf, offset)
            .map_err(|e| SinkError(format!("read_at {offset}: {e}")))
    }
}

#[cfg(unix)]
fn prepare_sources(
    paths: Vec<PathBuf>,
) -> Result<Vec<(String, DiskSource, Option<mouser_files::Hash>)>, String> {
    if paths.is_empty() {
        return Err("no files supplied".to_string());
    }
    paths
        .into_iter()
        .map(|path| {
            let name = safe_file_name(&path)?;
            let file =
                std::fs::File::open(&path).map_err(|e| format!("open {}: {e}", path.display()))?;
            let meta = file
                .metadata()
                .map_err(|e| format!("metadata {}: {e}", path.display()))?;
            if !meta.is_file() {
                return Err(format!("{} is not a regular file", path.display()));
            }
            let hash = hash_file(&file, meta.len())?;
            Ok((
                name,
                DiskSource {
                    file,
                    len: meta.len(),
                },
                Some(hash),
            ))
        })
        .collect()
}

#[cfg(unix)]
fn safe_file_name(path: &Path) -> Result<String, String> {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| format!("{} has no UTF-8 file name", path.display()))?;
    mouser_files::sanitize_name(name).map_err(|e| format!("unsafe file name {name:?}: {e}"))?;
    Ok(name.to_string())
}

#[cfg(unix)]
fn hash_file(file: &std::fs::File, len: u64) -> Result<mouser_files::Hash, String> {
    use sha2::{Digest, Sha256};
    use std::os::unix::fs::FileExt;

    let mut hasher = Sha256::new();
    let mut buf = [0u8; HASH_BUF];
    let mut offset = 0u64;
    while offset < len {
        let want = usize::try_from(len - offset)
            .map(|remaining| remaining.min(buf.len()))
            .unwrap_or(buf.len());
        let slot = buf
            .get_mut(..want)
            .ok_or_else(|| "hash buffer range out of bounds".to_string())?;
        let n = file
            .read_at(slot, offset)
            .map_err(|e| format!("hash read_at {offset}: {e}"))?;
        if n == 0 {
            return Err(format!("short read while hashing at byte {offset}"));
        }
        let bytes = slot
            .get(..n)
            .ok_or_else(|| "hash read length out of bounds".to_string())?;
        hasher.update(bytes);
        offset = offset.saturating_add(n as u64);
    }
    Ok(hasher.finalize().into())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use mouser_files::sha256;

    fn scratch_dir(tag: &str) -> PathBuf {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock after epoch")
            .as_nanos();
        std::env::temp_dir().join(format!("mouser-engine-{tag}-{}-{now}", std::process::id()))
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn daemon_bulk_loopback_sends_file_into_quarantine() {
        let receiver_id = DeviceIdentity::generate();
        let sender_id = DeviceIdentity::generate();
        let receiver_device = receiver_id.device_id();
        let sender_device = sender_id.device_id();

        let receiver = BulkEndpoint::bind_server(
            &receiver_id,
            mouser_net::loopback_addr(),
            PinPolicy::Pinned(sender_device),
        )
        .expect("bind receiver");
        let receiver_addr = receiver.local_addr().expect("receiver addr");
        let sender = BulkEndpoint::bind_client(mouser_net::loopback_addr()).expect("bind sender");

        let quarantine = scratch_dir("quarantine");
        let source_dir = scratch_dir("source");
        std::fs::create_dir_all(&quarantine).expect("create quarantine");
        std::fs::create_dir_all(&source_dir).expect("create source dir");
        let source_path = source_dir.join("sample.txt");
        let bytes = b"mouser daemon file transfer\nsmall but real\n".to_vec();
        std::fs::write(&source_path, &bytes).expect("write source");
        let expected_hash = sha256(&bytes);

        let recv_task = tokio::spawn({
            let quarantine = quarantine.clone();
            async move {
                let conn = receiver.accept_bulk(BULK_SESSION_ID).await.expect("accept");
                serve_bulk_connection(conn, quarantine).await
            }
        });

        send_paths_to_addr(
            &sender,
            &sender_id,
            receiver_device,
            receiver_addr,
            vec![source_path],
        )
        .await
        .expect("send file");

        let landed = quarantine.join("sample.txt");
        for _ in 0..200 {
            if landed.exists() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        let got = std::fs::read(&landed).expect("read landed file");
        assert_eq!(got, bytes);
        assert_eq!(sha256(&got), expected_hash);

        sender.close();
        recv_task.abort();
        let _ = std::fs::remove_dir_all(&quarantine);
        let _ = std::fs::remove_dir_all(&source_dir);
    }
}
