//! The IPC bridge: a [`mouser_ipc::Server`] that publishes a snapshot of the shared
//! discovery registry (peers + trust) and connection state, republishing on every
//! registry change, plus the channels the serve loop uses to learn about UI
//! `Connect`/`Disconnect` commands and to report connection state.

use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Instant;

use mouser_core::DeviceId;
use mouser_ipc::{
    Command, ConnectionDto, ConnectionStateDto, DeviceDto, PairingDto, PeerDto, Publisher, Server,
    SettingsDto, Snapshot,
};
use tokio::sync::mpsc;

use crate::daemon_store::DaemonStore;
use crate::discovery::{self, PeerRegistry};

/// OS kind advertised for the local device DTO (matches the frontend `OsKind`).
const OS_KIND: &str = if cfg!(target_os = "macos") {
    "macos"
} else if cfg!(target_os = "windows") {
    "windows"
} else {
    "linux"
};

/// A UI `Connect` request forwarded to the serve loop: the trusted peer to dial, plus an
/// optional address the desktop already resolved over its own browse (when present the
/// engine dials it directly; otherwise the engine resolves the id from its registry).
pub(super) struct ConnectRequest {
    pub peer_id: DeviceId,
}

/// A UI `SendFiles` request resolved to the currently active peer.
pub(super) struct FileSendRequest {
    pub peer_id: DeviceId,
    pub paths: Vec<PathBuf>,
}

/// Shared, mutable engine state the snapshot is built from.
struct Shared {
    store: DaemonStore,
    local: DeviceDto,
    /// The host-wide discovery registry (one browse for the whole daemon).
    registry: PeerRegistry,
    connection: Mutex<ConnectionDto>,
    /// A pending inbound pairing request awaiting the user's Approve/Deny, if any.
    pairing: Mutex<Option<PairingDto>>,
    /// Daemon-owned, persisted settings, surfaced in every snapshot and updated via
    /// [`Command::UpdateSettings`] (the single source of truth for UI + MCP).
    settings: Mutex<SettingsDto>,
    /// When the bridge started, for time-based health checks (e.g. only warn about
    /// finding no peers after a startup grace window, to avoid flapping).
    started: Instant,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The running IPC bridge handle the serve loop drives.
pub struct IpcBridge {
    shared: Arc<Shared>,
    /// Cheap publish handle (no lock contention with the command-receiving task).
    publisher: Publisher,
    // `tokio::sync::Mutex` so the single consumer (the serve loop) can hold the
    // guard across the `recv().await` without breaking `Send`.
    connect_rx: tokio::sync::Mutex<mpsc::UnboundedReceiver<ConnectRequest>>,
    disconnect_rx: tokio::sync::Mutex<mpsc::UnboundedReceiver<()>>,
    file_send_rx: tokio::sync::Mutex<mpsc::UnboundedReceiver<FileSendRequest>>,
    /// Approve/Deny decisions for pending pairings: `(peer_id base32, approved)`.
    decision_rx: tokio::sync::Mutex<mpsc::UnboundedReceiver<(String, bool)>>,
    tasks: Vec<tokio::task::JoinHandle<()>>,
}

/// Cloneable read handle for daemon-owned settings.
#[derive(Clone)]
pub(super) struct SettingsSource {
    shared: Arc<Shared>,
}

impl SettingsSource {
    pub(super) fn settings(&self) -> SettingsDto {
        lock(&self.shared.settings).clone()
    }
}

impl IpcBridge {
    /// Start the bridge: bind the IPC server, spawn the republish + command loops.
    pub async fn start(
        store: DaemonStore,
        local_id: String,
        local_name: String,
        registry: PeerRegistry,
    ) -> Result<Self, String> {
        let settings = store.load_settings();
        let shared = Arc::new(Shared {
            store,
            local: DeviceDto {
                id: local_id,
                name: local_name,
                os: OS_KIND.to_string(),
            },
            registry,
            connection: Mutex::new(ConnectionDto::default()),
            pairing: Mutex::new(None),
            settings: Mutex::new(settings),
            started: Instant::now(),
        });

        let server = Server::bind(build_snapshot(&shared))
            .await
            .map_err(|e| e.to_string())?;
        crate::diag!(
            info,
            "mouserd: IPC listening at {}",
            server.socket_path().display()
        );
        let publisher = server.publisher();

        let (connect_tx, connect_rx) = mpsc::unbounded_channel();
        let (disconnect_tx, disconnect_rx) = mpsc::unbounded_channel();
        let (file_send_tx, file_send_rx) = mpsc::unbounded_channel();
        let (decision_tx, decision_rx) = mpsc::unbounded_channel();

        // The command loop owns the `Server` (it awaits `recv_command`); the bridge
        // and republish loop publish through cloned `Publisher`s, so reporting state
        // never contends with command reception.
        let tasks = vec![
            tokio::spawn(republish_loop(Arc::clone(&shared), publisher.clone())),
            tokio::spawn(command_loop(
                server,
                Arc::clone(&shared),
                publisher.clone(),
                connect_tx,
                disconnect_tx,
                file_send_tx,
                decision_tx,
            )),
        ];

        Ok(Self {
            shared,
            publisher,
            connect_rx: tokio::sync::Mutex::new(connect_rx),
            disconnect_rx: tokio::sync::Mutex::new(disconnect_rx),
            file_send_rx: tokio::sync::Mutex::new(file_send_rx),
            decision_rx: tokio::sync::Mutex::new(decision_rx),
            tasks,
        })
    }

    /// Await the next UI `Connect` request (decoded peer id + optional resolved addr).
    pub async fn next_connect_request(&self) -> Option<ConnectRequest> {
        // The receiver is single-consumer; the serve loop is the only caller.
        let mut guard = self.connect_rx.lock().await;
        guard.recv().await
    }

    /// Await the next UI `Disconnect` request.
    pub async fn next_disconnect_request(&self) {
        let mut guard = self.disconnect_rx.lock().await;
        let _ = guard.recv().await;
    }

    /// Await the next UI file-send request for the active peer.
    pub async fn next_file_send_request(&self) -> Option<FileSendRequest> {
        let mut guard = self.file_send_rx.lock().await;
        guard.recv().await
    }

    /// Cloneable settings source for session tasks.
    pub(super) fn settings_source(&self) -> SettingsSource {
        SettingsSource {
            shared: Arc::clone(&self.shared),
        }
    }

    /// Report that the engine connected to `peer_id`; republish the snapshot.
    pub fn set_connected(&self, peer_id: &str, owner_id: &str) {
        *lock(&self.shared.connection) = ConnectionDto {
            state: ConnectionStateDto::Connected,
            peer_id: Some(peer_id.to_string()),
            owner: Some(owner_id.to_string()),
            epoch: None,
            error: None,
        };
        self.republish();
    }

    /// Report that a dial to `peer_id` is in progress; republish the snapshot so the
    /// UI can show "connecting" and clear any prior failure.
    pub fn set_connecting(&self, peer_id: &str) {
        *lock(&self.shared.connection) = ConnectionDto {
            state: ConnectionStateDto::Connecting,
            peer_id: Some(peer_id.to_string()),
            owner: None,
            epoch: None,
            error: None,
        };
        self.republish();
    }

    /// Report that the last connection attempt failed with `reason`; republish so the
    /// UI can explain the failure instead of silently returning to idle.
    pub fn set_connect_error(&self, reason: &str) {
        *lock(&self.shared.connection) = ConnectionDto {
            state: ConnectionStateDto::Idle,
            peer_id: None,
            owner: None,
            epoch: None,
            error: Some(reason.to_string()),
        };
        self.republish();
    }

    /// Report that the engine has no connection; republish the snapshot.
    pub fn set_idle(&self) {
        *lock(&self.shared.connection) = ConnectionDto::default();
        self.republish();
    }

    /// Surface a pending inbound pairing request to connected UIs (Allow/Deny prompt),
    /// naming the device that asked to connect.
    pub fn request_pairing(&self, peer_id: String, name: String, sas: String) {
        *lock(&self.shared.pairing) = Some(PairingDto { peer_id, name, sas });
        self.republish();
    }

    /// Clear the pending pairing request (decided, timed out, or the dialer left).
    pub fn clear_pairing(&self) {
        *lock(&self.shared.pairing) = None;
        self.republish();
    }

    /// Await the next pairing Approve/Deny decision from a UI: `(peer_id base32, approved)`.
    pub async fn next_pairing_decision(&self) -> Option<(String, bool)> {
        let mut guard = self.decision_rx.lock().await;
        guard.recv().await
    }

    fn republish(&self) {
        self.publisher.publish(build_snapshot(&self.shared));
    }
}

impl Drop for IpcBridge {
    fn drop(&mut self) {
        for task in self.tasks.drain(..) {
            task.abort();
        }
    }
}

/// Build a fresh snapshot from the shared state (local + discovered peers + trust).
fn build_snapshot(shared: &Shared) -> Snapshot {
    let mut peers: Vec<PeerDto> = shared
        .registry
        .peers()
        .into_iter()
        .filter(|advert| advert.id != shared.local.id) // never list ourselves
        .map(|advert| {
            let trusted = discovery::peer_device_id(&advert)
                .map(|id| shared.store.is_peer_trusted(&id).unwrap_or(false))
                .unwrap_or(false);
            let host = advert
                .addrs
                .first()
                .map(|ip| ip.to_string())
                .unwrap_or_default();
            PeerDto {
                name: if advert.name.is_empty() {
                    host.clone()
                } else {
                    advert.name.clone()
                },
                id: advert.id,
                os: advert.os,
                host,
                port: advert.iport,
                trusted,
            }
        })
        .collect();
    peers.sort_by(|a, b| a.id.cmp(&b.id));
    let connection = lock(&shared.connection).clone();
    let diagnostics =
        super::ipc_health::build_diagnostics(shared.started, &peers, connection.error.as_deref());
    Snapshot {
        local: shared.local.clone(),
        peers,
        connection,
        pairing: lock(&shared.pairing).clone(),
        settings: lock(&shared.settings).clone(),
        diagnostics,
    }
}

/// Republish a fresh snapshot whenever the shared discovery registry changes, so
/// connected UIs see peers appear/leave live. Publishes once up front (publish-then-wait)
/// because the registry's browse runs before this loop subscribes, so peers discovered in
/// that startup window are already folded in and would otherwise not surface until the
/// next change (the bind-time snapshot was built from an empty registry).
async fn republish_loop(shared: Arc<Shared>, publisher: Publisher) {
    let mut changes = shared.registry.subscribe();
    loop {
        publisher.publish(build_snapshot(&shared));
        if changes.changed().await.is_err() {
            break; // registry sender dropped
        }
    }
}

/// Drain UI commands from the IPC server. Forward Connect/Disconnect to the serve
/// loop; handle Trust inline against the shared store (then republish so the UI sees
/// the new trust immediately). `GetSnapshot` is handled inside the server itself.
async fn command_loop(
    mut server: Server,
    shared: Arc<Shared>,
    publisher: Publisher,
    connect_tx: mpsc::UnboundedSender<ConnectRequest>,
    disconnect_tx: mpsc::UnboundedSender<()>,
    file_send_tx: mpsc::UnboundedSender<FileSendRequest>,
    decision_tx: mpsc::UnboundedSender<(String, bool)>,
) {
    loop {
        match server.recv_command().await {
            Some(Command::Connect { peer_id }) => match discovery::decode_device_id(&peer_id) {
                Some(id) => match connect_request(&shared, id) {
                    Ok(request) => {
                        let _ = connect_tx.send(request);
                    }
                    Err(reason) => {
                        crate::diag!(
                            info,
                            "mouserd: IPC Connect rejected for {peer_id}: {reason}"
                        );
                        publish_connect_error(&shared, &publisher, &reason);
                    }
                },
                None => crate::diag!(info, "mouserd: IPC Connect with invalid peer id: {peer_id}"),
            },
            Some(Command::Disconnect) => {
                let _ = disconnect_tx.send(());
            }
            Some(Command::Trust { peer_id }) => match discovery::decode_device_id(&peer_id) {
                Some(id) => match shared.store.trust_peer(id) {
                    Ok(()) => {
                        crate::diag!(info, "mouserd: trusted peer {peer_id} (paired via IPC)");
                        // Rebuild + push so the UI flips the peer to "paired" at once;
                        // the cached snapshot would otherwise not reflect the new trust.
                        publisher.publish(build_snapshot(&shared));
                    }
                    Err(e) => crate::diag!(info, "mouserd: failed to trust peer {peer_id}: {e}"),
                },
                None => crate::diag!(info, "mouserd: IPC Trust with invalid peer id: {peer_id}"),
            },
            Some(Command::ApprovePairing { peer_id }) => {
                let _ = decision_tx.send((peer_id, true));
            }
            Some(Command::DenyPairing { peer_id }) => {
                let _ = decision_tx.send((peer_id, false));
            }
            Some(Command::SendFiles { paths }) => match file_send_request(&shared, paths) {
                Ok(request) => {
                    let _ = file_send_tx.send(request);
                }
                Err(reason) => crate::diag!(info, "mouserd: IPC SendFiles rejected: {reason}"),
            },
            Some(Command::UpdateSettings { settings }) => {
                if let Err(e) = shared.store.save_settings(&settings) {
                    crate::diag!(info, "mouserd: failed to persist settings: {e}");
                }
                *lock(&shared.settings) = settings;
                // Republish so every connected surface (UI + MCP) reflects it at once.
                publisher.publish(build_snapshot(&shared));
            }
            Some(Command::ResetData) => match shared.store.reset_data() {
                Ok(()) => {
                    // Drop any active session: the peer is no longer trusted after a
                    // reset, so a still-open connection to it must not linger. Signalling
                    // unconditionally is harmless when already idle (the plain Disconnect
                    // path does the same).
                    let _ = disconnect_tx.send(());
                    // Settings revert to defaults in memory too; peer `trusted` flags are
                    // recomputed from the (now empty) store on the next build_snapshot.
                    *lock(&shared.settings) = SettingsDto::default();
                    publisher.publish(build_snapshot(&shared));
                    crate::diag!(info, "mouserd: reset — cleared trusted peers and settings");
                }
                // Disk reset failed: leave in-memory state untouched so the snapshot keeps
                // reporting the real (unchanged) trust + settings — the UI then shows the
                // reset did not take, rather than a false success — and it can be retried.
                Err(e) => crate::diag!(info, "mouserd: reset failed, store left unchanged: {e}"),
            },
            // GetSnapshot is answered by the server; nothing reaches here.
            Some(Command::GetSnapshot) => {}
            None => return, // server dropped
        }
    }
}

fn file_send_request(shared: &Shared, paths: Vec<String>) -> Result<FileSendRequest, String> {
    let connection = lock(&shared.connection);
    if connection.state != ConnectionStateDto::Connected {
        return Err("no active peer connection".to_string());
    }
    let peer_text = connection
        .peer_id
        .as_ref()
        .ok_or_else(|| "connected state has no peer id".to_string())?;
    let peer_id = discovery::decode_device_id(peer_text)
        .ok_or_else(|| "active peer id is malformed".to_string())?;
    Ok(FileSendRequest {
        peer_id,
        paths: paths.into_iter().map(PathBuf::from).collect(),
    })
}

/// Validate a requested peer (in the live registry + trusted) before queuing the dial.
/// The engine resolves the address(es) itself, so no caller-supplied host/port is taken.
fn connect_request(shared: &Shared, peer_id: DeviceId) -> Result<ConnectRequest, String> {
    if shared.registry.find(&peer_id).is_none() {
        return Err("peer is not in the live discovery registry".to_string());
    }
    match shared.store.is_peer_trusted(&peer_id) {
        Ok(true) => {}
        Ok(false) => return Err("peer is not trusted".to_string()),
        Err(e) => return Err(format!("trust check failed: {e}")),
    }
    Ok(ConnectRequest { peer_id })
}

fn publish_connect_error(shared: &Shared, publisher: &Publisher, reason: &str) {
    *lock(&shared.connection) = ConnectionDto {
        state: ConnectionStateDto::Idle,
        peer_id: None,
        owner: None,
        epoch: None,
        error: Some(reason.to_string()),
    };
    publisher.publish(build_snapshot(shared));
}

#[cfg(test)]
#[path = "ipc_bridge_tests.rs"]
mod tests;
