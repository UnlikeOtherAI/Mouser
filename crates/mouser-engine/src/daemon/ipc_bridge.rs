//! The IPC bridge: a [`mouser_ipc::Server`] that publishes a snapshot of the shared
//! discovery registry (peers + trust) and connection state, republishing on every
//! registry change, plus the channels the serve loop uses to learn about UI
//! `Connect`/`Disconnect` commands and to report connection state.

use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use mouser_core::DeviceId;
use mouser_ipc::{
    Command, ConnectionDto, ConnectionStateDto, DeviceDto, PeerDto, Publisher, Server, Snapshot,
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
    pub addr: Option<SocketAddr>,
}

/// Shared, mutable engine state the snapshot is built from.
struct Shared {
    store: DaemonStore,
    local: DeviceDto,
    /// The host-wide discovery registry (one browse for the whole daemon).
    registry: PeerRegistry,
    connection: Mutex<ConnectionDto>,
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
    tasks: Vec<tokio::task::JoinHandle<()>>,
}

impl IpcBridge {
    /// Start the bridge: bind the IPC server, spawn the republish + command loops.
    pub async fn start(
        store: DaemonStore,
        local_id: String,
        local_name: String,
        registry: PeerRegistry,
    ) -> Result<Self, String> {
        let shared = Arc::new(Shared {
            store,
            local: DeviceDto {
                id: local_id,
                name: local_name,
                os: OS_KIND.to_string(),
            },
            registry,
            connection: Mutex::new(ConnectionDto::default()),
        });

        let server = Server::bind(build_snapshot(&shared))
            .await
            .map_err(|e| e.to_string())?;
        eprintln!(
            "mouserd: IPC listening at {}",
            server.socket_path().display()
        );
        let publisher = server.publisher();

        let (connect_tx, connect_rx) = mpsc::unbounded_channel();
        let (disconnect_tx, disconnect_rx) = mpsc::unbounded_channel();

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
            )),
        ];

        Ok(Self {
            shared,
            publisher,
            connect_rx: tokio::sync::Mutex::new(connect_rx),
            disconnect_rx: tokio::sync::Mutex::new(disconnect_rx),
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
    Snapshot {
        local: shared.local.clone(),
        peers,
        connection: lock(&shared.connection).clone(),
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
) {
    loop {
        match server.recv_command().await {
            Some(Command::Connect {
                peer_id,
                host,
                port,
            }) => match discovery::decode_device_id(&peer_id) {
                Some(id) => {
                    let addr = connect_addr(host, port);
                    let _ = connect_tx.send(ConnectRequest { peer_id: id, addr });
                }
                None => eprintln!("mouserd: IPC Connect with invalid peer id: {peer_id}"),
            },
            Some(Command::Disconnect) => {
                let _ = disconnect_tx.send(());
            }
            Some(Command::Trust { peer_id }) => match discovery::decode_device_id(&peer_id) {
                Some(id) => match shared.store.trust_peer(id) {
                    Ok(()) => {
                        eprintln!("mouserd: trusted peer {peer_id} (paired via IPC)");
                        // Rebuild + push so the UI flips the peer to "paired" at once;
                        // the cached snapshot would otherwise not reflect the new trust.
                        publisher.publish(build_snapshot(&shared));
                    }
                    Err(e) => eprintln!("mouserd: failed to trust peer {peer_id}: {e}"),
                },
                None => eprintln!("mouserd: IPC Trust with invalid peer id: {peer_id}"),
            },
            // GetSnapshot is answered by the server; nothing reaches here.
            Some(Command::GetSnapshot) => {}
            None => return, // server dropped
        }
    }
}

/// Pair an optional desktop-supplied host + port into a dialable [`SocketAddr`]. Returns
/// `None` (engine resolves the id from its own registry) unless both are present and the
/// host parses as an IP.
fn connect_addr(host: Option<String>, port: Option<u16>) -> Option<SocketAddr> {
    let (Some(host), Some(port)) = (host, port) else {
        return None;
    };
    let ip: IpAddr = match host.parse() {
        Ok(ip) => ip,
        Err(e) => {
            eprintln!("mouserd: IPC Connect with invalid host {host}: {e}");
            return None;
        }
    };
    Some(SocketAddr::new(ip, port))
}
