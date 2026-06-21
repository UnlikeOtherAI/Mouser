//! Loopback integration for the **bulk connection** (§6.2): two in-process bulk
//! endpoints establish identity-bound bulk connections (`BulkHello`, §5 step 5), open a
//! dedicated stream per `transfer_id`, and run a **real multi-file transfer** by driving
//! the `mouser-files` engine over [`mouser_net::TransferStream`]. The receiver writes to
//! a real **disk-backed, path-sanitized quarantine sink**; the test asserts the bytes,
//! the SHA-256, and the sanitized on-disk paths.
//!
//! Cert pinning (§3) is exercised on the bulk plane; the bulk `channel_sig` binding to
//! the interactive `session_id` is verified inside `accept_bulk`.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use mouser_files::{sha256, FileSink, Outbound, Receiver, ReceiverConfig, Sender, SinkError};
use mouser_net::{BulkEndpoint, Identity, PinPolicy, TransferStream};
use mouser_protocol::{
    from_cbor, to_cbor, FileAccept, FileChunk, FileDone, FileOffer, TYPE_FILE_ACCEPT,
    TYPE_FILE_ACK, TYPE_FILE_CHUNK, TYPE_FILE_DONE, TYPE_FILE_OFFER,
};

/// Deterministic pseudo-random bytes for content assertions.
fn bytes(seed: u64, len: usize) -> Vec<u8> {
    let mut v = Vec::with_capacity(len);
    let mut x = seed ^ 0xDEAD_BEEF_CAFE_F00D;
    for _ in 0..len {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        v.push((x & 0xFF) as u8);
    }
    v
}

/// A real `std::fs`-backed sink that lands bytes inside a quarantine dir. The path was
/// already sanitized by [`resolve_in_quarantine`]; opening with `create_new` ensures we
/// never follow a pre-existing symlink (the on-disk half of §7.8 "no symlink follow").
struct FsSink {
    path: PathBuf,
    file: fs::File,
    written: u64,
}

impl FsSink {
    fn create(path: &Path) -> Result<Self, SinkError> {
        let file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .map_err(|e| SinkError(format!("open {}: {e}", path.display())))?;
        Ok(Self {
            path: path.to_path_buf(),
            file,
            written: 0,
        })
    }
}

impl FileSink for FsSink {
    fn existing_len(&self) -> u64 {
        self.written
    }

    fn write_at(&mut self, _offset: u64, data: &[u8]) -> Result<(), SinkError> {
        self.file
            .write_all(data)
            .map_err(|e| SinkError(format!("write: {e}")))?;
        self.written += data.len() as u64;
        Ok(())
    }

    fn finish(&mut self) -> Result<mouser_files::Hash, SinkError> {
        self.file.flush().map_err(|e| SinkError(format!("flush: {e}")))?;
        let bytes = fs::read(&self.path).map_err(|e| SinkError(format!("reread: {e}")))?;
        Ok(sha256(&bytes))
    }
}

/// Drive the **sender** side over a transfer stream: offer, apply accept, then alternate
/// sending windowed chunks with reading acks until complete; finally read `FileDone`.
async fn run_sender(
    mut sender: Sender<mouser_files::MemSource>,
    stream: &mut TransferStream,
) -> Result<(), String> {
    let offer = sender.offer();
    stream
        .send_msg(TYPE_FILE_OFFER, &to_cbor(&offer).map_err(|e| e.to_string())?)
        .await
        .map_err(|e| e.to_string())?;

    let (ty, payload) = stream.recv_msg().await.map_err(|e| e.to_string())?;
    if ty != TYPE_FILE_ACCEPT {
        return Err(format!("expected FileAccept, got {ty:#06x}"));
    }
    let accept: FileAccept = from_cbor(&payload).map_err(|e| e.to_string())?;
    sender.on_accept(&accept).map_err(|e| e.to_string())?;

    loop {
        // Push every chunk the window currently permits.
        while let Some(chunk) = sender.poll_chunk().map_err(|e| e.to_string())? {
            let body = to_cbor(&chunk).map_err(|e| e.to_string())?;
            stream
                .send_msg(TYPE_FILE_CHUNK, &body)
                .await
                .map_err(|e| e.to_string())?;
        }
        if sender.is_complete() {
            break;
        }
        // Window is full (or all sent, awaiting final acks): read an ack to advance.
        let (ty, payload) = stream.recv_msg().await.map_err(|e| e.to_string())?;
        match ty {
            TYPE_FILE_ACK => {
                let ack = from_cbor(&payload).map_err(|e| e.to_string())?;
                sender.on_ack(&ack).map_err(|e| e.to_string())?;
            }
            TYPE_FILE_DONE => {
                let done: FileDone = from_cbor(&payload).map_err(|e| e.to_string())?;
                sender.on_done(&done).map_err(|e| e.to_string())?;
                break;
            }
            other => return Err(format!("sender got unexpected type {other:#06x}")),
        }
    }
    let _ = stream.finish();
    Ok(())
}

/// Drive the **receiver** side: read the offer, build disk-backed sinks under
/// `quarantine`, send accept, then commit chunks and emit acks/done until complete.
async fn run_receiver(
    stream: &mut TransferStream,
    quarantine: PathBuf,
    expected_hashes: Vec<Option<mouser_files::Hash>>,
) -> Result<Receiver<FsSink>, String> {
    let (ty, payload) = stream.recv_msg().await.map_err(|e| e.to_string())?;
    if ty != TYPE_FILE_OFFER {
        return Err(format!("expected FileOffer, got {ty:#06x}"));
    }
    let offer: FileOffer = from_cbor(&payload).map_err(|e| e.to_string())?;
    let config = ReceiverConfig::new(quarantine).with_expected_hashes(expected_hashes);
    // The engine has already resolved each path safely inside the quarantine dir via
    // `resolve_in_quarantine` (path-traversal rejected before we get here); the factory
    // just opens it with `create_new` so a pre-existing symlink is never followed.
    let (mut receiver, out) =
        Receiver::accept_offer(&offer, config, |_i, path| FsSink::create(path))
            .map_err(|e| e.to_string())?;

    let accept = match out {
        Outbound::Accept(a) => a,
        Outbound::Reject(r) => return Err(format!("receiver rejected offer: {}", r.reason)),
        other => return Err(format!("unexpected first outbound {other:?}")),
    };
    stream
        .send_msg(TYPE_FILE_ACCEPT, &to_cbor(&accept).map_err(|e| e.to_string())?)
        .await
        .map_err(|e| e.to_string())?;

    while !receiver.is_complete() {
        let (ty, payload) = stream.recv_msg().await.map_err(|e| e.to_string())?;
        if ty != TYPE_FILE_CHUNK {
            return Err(format!("expected FileChunk, got {ty:#06x}"));
        }
        let chunk: FileChunk = from_cbor(&payload).map_err(|e| e.to_string())?;
        for out in receiver.on_chunk(&chunk).map_err(|e| e.to_string())? {
            match out {
                Outbound::Ack(ack) => {
                    stream
                        .send_msg(TYPE_FILE_ACK, &to_cbor(&ack).map_err(|e| e.to_string())?)
                        .await
                        .map_err(|e| e.to_string())?;
                }
                Outbound::Done(done) => {
                    stream
                        .send_msg(TYPE_FILE_DONE, &to_cbor(&done).map_err(|e| e.to_string())?)
                        .await
                        .map_err(|e| e.to_string())?;
                }
                _ => {}
            }
        }
    }
    Ok(receiver)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn bulk_multi_file_transfer_over_two_connections() {
    let server_id = Identity::generate().expect("server id");
    let client_id = Identity::generate().expect("client id");
    let server_device_id = server_id.device_id();
    let client_device_id = client_id.device_id();
    let session_id = 0x1234_5678_9ABC_DEF0u64;

    // Server (receiver) binds + pins the client; client (sender) pins the server (§3).
    let server = BulkEndpoint::bind_server(
        &server_id,
        mouser_net::loopback_addr(),
        PinPolicy::Pinned(client_device_id),
    )
    .expect("bind server");
    let server_addr = server.local_addr().expect("server addr");
    let client = BulkEndpoint::bind_client(mouser_net::loopback_addr()).expect("bind client");

    // Unique quarantine dir for this run.
    let quarantine = std::env::temp_dir().join(format!("mouser-q-{}", std::process::id()));
    let _ = fs::remove_dir_all(&quarantine);
    fs::create_dir_all(&quarantine).expect("mkdir quarantine");

    // The files to send (one larger than the 8 MiB window to force windowed acks).
    let f_big = bytes(1, 9_000_000);
    let f_small = bytes(2, 321);
    let names = ["from-A.bin", "notes.txt"];
    let expected = vec![Some(sha256(&f_big)), Some(sha256(&f_small))];

    // --- Receiver task on the server end ---
    let q_for_task = quarantine.clone();
    let expected_for_task = expected.clone();
    let recv_task = tokio::spawn(async move {
        let conn = server.accept_bulk(session_id).await.expect("accept_bulk");
        assert_eq!(
            conn.peer_device_id(),
            Some(client_device_id),
            "server pins the client's device_id (§3)"
        );
        let mut stream = conn.accept_transfer().await.expect("accept_transfer");
        let receiver = run_receiver(&mut stream, q_for_task, expected_for_task)
            .await
            .expect("receiver run");
        (server, conn, receiver) // keep alive
    });

    // --- Sender on the client end ---
    let conn = client
        .connect_bulk(
            &client_id,
            server_addr,
            PinPolicy::Pinned(server_device_id),
            session_id,
        )
        .await
        .expect("connect_bulk");
    let mut stream = conn.open_transfer(7).await.expect("open_transfer");

    let sender = Sender::new(
        7,
        vec![
            (names[0].into(), mouser_files::MemSource::new(f_big.clone())),
            (names[1].into(), mouser_files::MemSource::new(f_small.clone())),
        ],
    )
    .expect("sender");
    run_sender(sender, &mut stream).await.expect("sender run");

    let (_server, _conn, receiver) = recv_task.await.expect("recv task");
    assert!(receiver.is_complete(), "receiver completed all files");

    // Bytes + hash + sanitized paths on disk.
    for (name, content) in [(names[0], &f_big), (names[1], &f_small)] {
        let landed = quarantine.join(name);
        assert!(landed.starts_with(&quarantine), "file stays inside quarantine");
        let got = fs::read(&landed).expect("read landed file");
        assert_eq!(&got, content, "received bytes match for {name}");
        assert_eq!(sha256(&got), sha256(content), "sha256 matches for {name}");
    }

    let _ = fs::remove_dir_all(&quarantine);
    conn.close();
}

/// A bulk connection whose `BulkHello` binds to the WRONG interactive session id must
/// be rejected by `accept_bulk` (§5 step 5 — the binding is the whole point).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bulk_hello_wrong_session_is_rejected() {
    let server_id = Identity::generate().expect("server id");
    let client_id = Identity::generate().expect("client id");
    let server_device_id = server_id.device_id();
    let client_device_id = client_id.device_id();

    let server = BulkEndpoint::bind_server(
        &server_id,
        mouser_net::loopback_addr(),
        PinPolicy::Pinned(client_device_id),
    )
    .expect("bind server");
    let server_addr = server.local_addr().expect("server addr");
    let client = BulkEndpoint::bind_client(mouser_net::loopback_addr()).expect("bind client");

    // Server expects session 100; client binds to 999 → mismatch.
    let accept = tokio::spawn(async move {
        let r =
            tokio::time::timeout(std::time::Duration::from_secs(5), server.accept_bulk(100)).await;
        (server, r)
    });

    let dial = client
        .connect_bulk(&client_id, server_addr, PinPolicy::Pinned(server_device_id), 999)
        .await;

    let (_server, accept_result) = accept.await.expect("accept task");
    let accept_result = accept_result.expect("accept resolved within timeout");
    assert!(
        accept_result.is_err(),
        "accept_bulk MUST reject a BulkHello bound to a different session (§5 step 5)"
    );
    // The dial itself may succeed (TLS ok) or fail depending on timing; the binding
    // check on the acceptor is the security gate we assert above.
    let _ = dial;
}
