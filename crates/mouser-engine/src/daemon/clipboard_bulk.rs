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
        let addr = discovery::peer_bulk_socket_addr(&advert).ok_or_else(|| {
            format!(
                "peer {} did not advertise a dialable bulk endpoint",
                advert.instance_name()
            )
        })?;
        let conn = self.connection_for(addr).await?;
        if let Err(e) = send_chunks_on_connection(conn.as_ref(), chunks).await {
            self.clear_connection().await;
            return Err(e);
        }
        Ok(())
    }

    async fn connection_for(&self, addr: SocketAddr) -> Result<Arc<BulkConnection>, String> {
        if let Some(conn) = self.connection.lock().await.as_ref().cloned() {
            return Ok(conn);
        }
        let conn = Arc::new(
            self.endpoint
                .connect_bulk(
                    self.identity.as_ref(),
                    addr,
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
    publish_chunk(&tx, peer_id, first);

    while !last {
        let (ty, payload) = stream.recv_msg().await.map_err(|e| e.to_string())?;
        if ty != TYPE_CLIPBOARD_DATA {
            return Err(format!("expected ClipboardData, got {ty:#06x}"));
        }
        let chunk: ClipboardData = from_cbor(&payload).map_err(|e| e.to_string())?;
        validate_chunk(&chunk, &hash, format)?;
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
