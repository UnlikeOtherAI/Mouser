//! Bulk-plane clipboard stream helpers.

use std::net::SocketAddr;
use std::sync::Arc;

use mouser_clipboard::MAX_DATA_CHUNK;
use mouser_core::{DeviceId, DeviceIdentity};
use mouser_net::{BulkConnection, BulkEndpoint, PinPolicy, TransferStream};
use mouser_protocol::{from_cbor, to_cbor, ClipboardData, TYPE_CLIPBOARD_DATA};
use tokio::sync::{mpsc, Mutex};

use crate::discovery::{self, PeerRegistry};

use super::file_transfer::BULK_SESSION_ID;

/// Upper bound on a single inbound clipboard transfer's reassembled size. Each chunk is
/// already capped at [`MAX_DATA_CHUNK`] (1 MiB), but the receive loop runs `while !last`
/// with no total bound — a (cert-pinned, active) peer that never sets `last` could stream
/// forever and exhaust memory. 256 MiB comfortably covers a large clipboard image while
/// bounding the blast radius.
const MAX_CLIPBOARD_BULK_TOTAL: u64 = 256 * 1024 * 1024;
/// Upper bound on chunk count for one transfer — guards the byte cap against a flood of
/// zero-/tiny-byte chunks (which would never trip a pure byte limit).
const MAX_CLIPBOARD_BULK_CHUNKS: u64 = 64 * 1024;

#[derive(Clone, Debug)]
pub(super) struct InboundClipboardData {
    pub peer_id: DeviceId,
    pub data: ClipboardData,
}

pub(super) type ClipboardBulkTx = mpsc::UnboundedSender<InboundClipboardData>;
pub(super) type ClipboardBulkRx = Arc<Mutex<mpsc::UnboundedReceiver<InboundClipboardData>>>;

pub(super) fn channel() -> (ClipboardBulkTx, ClipboardBulkRx) {
    let (tx, rx) = mpsc::unbounded_channel();
    (tx, Arc::new(Mutex::new(rx)))
}

#[derive(Clone)]
pub(super) struct BulkClipboardSender {
    endpoint: Arc<BulkEndpoint>,
    identity: Arc<DeviceIdentity>,
    registry: PeerRegistry,
    peer: DeviceId,
    connection: Arc<Mutex<Option<Arc<BulkConnection>>>>,
}

impl BulkClipboardSender {
    pub(super) fn new(
        endpoint: Arc<BulkEndpoint>,
        identity: Arc<DeviceIdentity>,
        registry: PeerRegistry,
        peer: DeviceId,
    ) -> Self {
        Self {
            endpoint,
            identity,
            registry,
            peer,
            connection: Arc::new(Mutex::new(None)),
        }
    }

    pub(super) async fn send_chunks(&self, chunks: Vec<ClipboardData>) -> Result<(), String> {
        let advert = self.registry.find(&self.peer).ok_or_else(|| {
            format!(
                "peer {} is not in the live discovery registry",
                crate::daemon_store::format_device_id(&self.peer)
            )
        })?;
        let addrs = discovery::peer_bulk_socket_addrs(&advert);
        if addrs.is_empty() {
            return Err(format!(
                "peer {} did not advertise a dialable bulk endpoint",
                advert.instance_name()
            ));
        }
        let conn = self.connection_for(&addrs).await?;
        if let Err(e) = send_chunks_on_connection(conn.as_ref(), chunks).await {
            self.clear_connection().await;
            return Err(e);
        }
        Ok(())
    }

    async fn connection_for(&self, addrs: &[SocketAddr]) -> Result<Arc<BulkConnection>, String> {
        if let Some(conn) = self.connection.lock().await.as_ref().cloned() {
            return Ok(conn);
        }
        let conn = Arc::new(
            self.endpoint
                .connect_bulk_any(
                    self.identity.as_ref(),
                    addrs,
                    PinPolicy::Pinned(self.peer),
                    BULK_SESSION_ID,
                )
                .await
                .map_err(|e| e.to_string())?,
        );
        let mut guard = self.connection.lock().await;
        if let Some(existing) = guard.as_ref() {
            return Ok(Arc::clone(existing));
        }
        *guard = Some(Arc::clone(&conn));
        Ok(conn)
    }

    async fn clear_connection(&self) {
        let mut guard = self.connection.lock().await;
        *guard = None;
    }
}

#[cfg(test)]
pub(super) async fn send_chunks_to_addr(
    endpoint: &BulkEndpoint,
    identity: &DeviceIdentity,
    peer: DeviceId,
    addr: SocketAddr,
    chunks: Vec<ClipboardData>,
) -> Result<BulkConnection, String> {
    if chunks.is_empty() {
        return endpoint
            .connect_bulk(identity, addr, PinPolicy::Pinned(peer), BULK_SESSION_ID)
            .await
            .map_err(|e| e.to_string());
    }
    let conn = endpoint
        .connect_bulk(identity, addr, PinPolicy::Pinned(peer), BULK_SESSION_ID)
        .await
        .map_err(|e| e.to_string())?;
    send_chunks_on_connection(&conn, chunks).await?;
    Ok(conn)
}

async fn send_chunks_on_connection(
    conn: &BulkConnection,
    chunks: Vec<ClipboardData>,
) -> Result<(), String> {
    let Some(first) = chunks.first() else {
        return Ok(());
    };
    validate_chunk(first, &first.hash, first.format)?;
    let hash = first.hash.clone();
    let format = first.format;
    let transfer_id = transfer_id_from_hash(&hash);
    let mut stream = conn
        .open_transfer(transfer_id)
        .await
        .map_err(|e| e.to_string())?;
    for chunk in chunks {
        validate_chunk(&chunk, &hash, format)?;
        let payload = to_cbor(&chunk).map_err(|e| e.to_string())?;
        stream
            .send_msg(TYPE_CLIPBOARD_DATA, &payload)
            .await
            .map_err(|e| e.to_string())?;
    }
    let _ = stream.finish();
    Ok(())
}

pub(super) async fn receive_clipboard_stream(
    mut stream: TransferStream,
    peer_id: DeviceId,
    first_payload: Vec<u8>,
    tx: ClipboardBulkTx,
) -> Result<(), String> {
    let first: ClipboardData = from_cbor(&first_payload).map_err(|e| e.to_string())?;
    validate_chunk(&first, &first.hash, first.format)?;
    let hash = first.hash.clone();
    let format = first.format;
    let mut last = first.last;
    let mut total: u64 = first.data.len() as u64;
    let mut chunks: u64 = 1;
    publish_chunk(&tx, peer_id, first);

    while !last {
        let (ty, payload) = stream.recv_msg().await.map_err(|e| e.to_string())?;
        if ty != TYPE_CLIPBOARD_DATA {
            return Err(format!("expected ClipboardData, got {ty:#06x}"));
        }
        let chunk: ClipboardData = from_cbor(&payload).map_err(|e| e.to_string())?;
        validate_chunk(&chunk, &hash, format)?;
        total = total.saturating_add(chunk.data.len() as u64);
        chunks = chunks.saturating_add(1);
        if total > MAX_CLIPBOARD_BULK_TOTAL {
            return Err("clipboard bulk transfer exceeded 256 MiB".to_string());
        }
        if chunks > MAX_CLIPBOARD_BULK_CHUNKS {
            return Err("clipboard bulk transfer exceeded chunk limit".to_string());
        }
        last = chunk.last;
        publish_chunk(&tx, peer_id, chunk);
    }
    let _ = stream.finish();
    Ok(())
}

fn validate_chunk(
    data: &ClipboardData,
    hash: &[u8],
    format: mouser_protocol::ClipFormat,
) -> Result<(), String> {
    if data.hash != hash {
        return Err("clipboard bulk stream changed hash mid-transfer".to_string());
    }
    if data.format != format {
        return Err("clipboard bulk stream changed format mid-transfer".to_string());
    }
    if data.data.len() > MAX_DATA_CHUNK {
        return Err("clipboard bulk chunk exceeded 1 MiB".to_string());
    }
    Ok(())
}

fn publish_chunk(tx: &ClipboardBulkTx, peer_id: DeviceId, data: ClipboardData) {
    let _ = tx.send(InboundClipboardData { peer_id, data });
}

fn transfer_id_from_hash(hash: &[u8]) -> u64 {
    hash.get(..8)
        .and_then(|bytes| bytes.try_into().ok())
        .map(u64::from_be_bytes)
        .unwrap_or(0)
}
