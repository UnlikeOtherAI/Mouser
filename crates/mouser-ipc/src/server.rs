//! Daemon-side IPC: accept UI clients, push [`Snapshot`]s on change and on request,
//! and forward [`Command`]s back to the engine.
//!
//! The daemon holds the latest snapshot in a [`tokio::sync::watch`] channel. The
//! [`Server`] accepts connections on the Unix-domain socket; each client gets a task
//! that (a) pushes the current snapshot immediately, (b) pushes every later snapshot
//! the daemon publishes, and (c) reads the client's commands and forwards them on an
//! [`mpsc`](tokio::sync::mpsc) channel the daemon drains. The snapshot-building and
//! command-handling logic lives in the daemon (it owns discovery/trust/connection);
//! this module is the transport.

use std::path::{Path, PathBuf};
use std::sync::Arc;

#[cfg(unix)]
use std::fs::Permissions;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

#[cfg(windows)]
use tokio::net::windows::named_pipe::{NamedPipeServer, ServerOptions};
#[cfg(unix)]
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{mpsc, watch, OwnedSemaphorePermit, Semaphore};

use crate::codec::{read_message, write_message, IpcError};
use crate::dto::{Command, ServerMessage, Snapshot};
use crate::path::default_socket_path;
#[cfg(windows)]
use crate::windows_security::{
    create_user_pipe, current_process_user_sid, verify_pipe_client_user, UserSid,
};

const MAX_CLIENTS: usize = 4;
const COMMAND_QUEUE_CAPACITY: usize = 32;

#[cfg(unix)]
type IpcServerStream = UnixStream;
#[cfg(windows)]
type IpcServerStream = NamedPipeServer;

/// A cheap, cloneable handle to publish snapshots without touching the [`Server`].
///
/// Lets the daemon split its two concerns: one task owns the [`Server`] and awaits
/// [`Server::recv_command`], while any number of [`Publisher`]s push fresh snapshots
/// concurrently (no lock contention with the command-receiving task).
#[derive(Clone)]
pub struct Publisher {
    snapshot_tx: watch::Sender<Snapshot>,
}

impl Publisher {
    /// Publish a new snapshot to every connected client (and to the value new clients
    /// see on connect). Cheap; a no-op effect when nothing changed.
    pub fn publish(&self, snapshot: Snapshot) {
        self.snapshot_tx.send_replace(snapshot);
    }
}

/// A handle the daemon uses to publish snapshots and receive UI commands.
pub struct Server {
    socket_path: PathBuf,
    /// Latest snapshot, watched by every connected client task.
    snapshot_tx: watch::Sender<Snapshot>,
    /// Commands forwarded from connected clients to the daemon.
    command_rx: mpsc::Receiver<Command>,
    accept_task: Option<tokio::task::JoinHandle<()>>,
}

impl Server {
    /// Bind the IPC server at the well-known socket path with an initial snapshot.
    pub async fn bind(initial: Snapshot) -> Result<Self, IpcError> {
        Self::bind_at(default_socket_path(), initial).await
    }

    /// Bind the IPC server at an explicit socket/pipe path (tests pass a temp path).
    ///
    /// A stale socket file from a previous run is removed first so re-binding after a
    /// crash succeeds on Unix sockets (named pipes have no filesystem entry).
    pub async fn bind_at(
        socket_path: impl Into<PathBuf>,
        initial: Snapshot,
    ) -> Result<Self, IpcError> {
        let socket_path = socket_path.into();
        #[cfg(unix)]
        crate::path::prepare_default_socket_parent(&socket_path).map_err(IpcError::Io)?;
        #[cfg(unix)]
        match std::fs::remove_file(&socket_path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(IpcError::Io(e)),
        }
        let listener = IpcListener::bind(socket_path.clone())?;
        let (snapshot_tx, _snapshot_rx) = watch::channel(initial);
        let (command_tx, command_rx) = mpsc::channel(COMMAND_QUEUE_CAPACITY);
        let client_slots = Arc::new(Semaphore::new(MAX_CLIENTS));

        let accept_task = tokio::spawn(accept_loop(
            listener,
            snapshot_tx.clone(),
            command_tx,
            client_slots,
        ));

        Ok(Self {
            socket_path,
            snapshot_tx,
            command_rx,
            accept_task: Some(accept_task),
        })
    }

    /// The socket path this server is bound to.
    pub fn socket_path(&self) -> &Path {
        &self.socket_path
    }

    /// Publish a new snapshot. Every connected client receives it; the value is also
    /// the one new clients see on connect. Cheap when unchanged (no clients = no-op).
    pub fn publish(&self, snapshot: Snapshot) {
        self.snapshot_tx.send_replace(snapshot);
    }

    /// A cloneable [`Publisher`] for pushing snapshots without holding the `Server`.
    pub fn publisher(&self) -> Publisher {
        Publisher {
            snapshot_tx: self.snapshot_tx.clone(),
        }
    }

    /// Receive the next command from any connected UI client (`None` once the server
    /// is dropped and all clients gone).
    pub async fn recv_command(&mut self) -> Option<Command> {
        self.command_rx.recv().await
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        if let Some(task) = self.accept_task.take() {
            task.abort();
        }
        // Unlink the socket so a later bind on the same path is clean.
        #[cfg(unix)]
        let _ = std::fs::remove_file(&self.socket_path);
    }
}

#[cfg(unix)]
struct IpcListener {
    inner: UnixListener,
}

#[cfg(unix)]
impl IpcListener {
    fn bind(path: PathBuf) -> Result<Self, IpcError> {
        let inner = UnixListener::bind(&path)?;
        if let Err(e) = std::fs::set_permissions(&path, Permissions::from_mode(0o600)) {
            let _ = std::fs::remove_file(&path);
            return Err(IpcError::Io(e));
        }
        Ok(Self { inner })
    }

    async fn accept(&mut self) -> Result<IpcServerStream, std::io::Error> {
        let (stream, _addr) = self.inner.accept().await?;
        verify_peer_uid(&stream)?;
        Ok(stream)
    }
}

#[cfg(windows)]
struct IpcListener {
    pipe_name: PathBuf,
    pending: NamedPipeServer,
    daemon_user_sid: UserSid,
}

#[cfg(windows)]
impl IpcListener {
    fn bind(pipe_name: PathBuf) -> Result<Self, IpcError> {
        let daemon_user_sid = current_process_user_sid()?;
        let pending = create_pipe_instance(&pipe_name, true, &daemon_user_sid)?;
        Ok(Self {
            pipe_name,
            pending,
            daemon_user_sid,
        })
    }

    async fn accept(&mut self) -> Result<IpcServerStream, std::io::Error> {
        self.pending.connect().await?;
        let next = match create_pipe_instance(&self.pipe_name, false, &self.daemon_user_sid) {
            Ok(next) => next,
            Err(e) => {
                let _ = self.pending.disconnect();
                return Err(e);
            }
        };
        let connected = std::mem::replace(&mut self.pending, next);
        if let Err(e) = verify_pipe_client_user(&connected, &self.daemon_user_sid) {
            let _ = connected.disconnect();
            return Err(e);
        }
        Ok(connected)
    }
}

#[cfg(windows)]
fn pipe_options(first_instance: bool) -> ServerOptions {
    let mut options = ServerOptions::new();
    options.reject_remote_clients(true);
    if first_instance {
        options.first_pipe_instance(true);
    }
    options
}

#[cfg(windows)]
fn create_pipe_instance(
    pipe_name: &Path,
    first_instance: bool,
    daemon_user_sid: &UserSid,
) -> Result<NamedPipeServer, std::io::Error> {
    // This pipe is the engine control plane: a client can trust devices, connect
    // sessions, and send files by local path. The DACL therefore mirrors the
    // Unix `0o600` socket mode: only the daemon process user's SID receives
    // read/write pipe access. An explicit DACL with no Everyone/AuthUsers ACE
    // means other local users have no allow entry, so they cannot open it.
    let options = pipe_options(first_instance);
    create_user_pipe(&options, pipe_name.as_os_str(), daemon_user_sid)
}

/// Accept connections forever, spawning a per-client task for each.
async fn accept_loop(
    mut listener: IpcListener,
    snapshot_tx: watch::Sender<Snapshot>,
    command_tx: mpsc::Sender<Command>,
    client_slots: Arc<Semaphore>,
) {
    loop {
        match listener.accept().await {
            Ok(stream) => {
                let permit = match client_slots.clone().try_acquire_owned() {
                    Ok(permit) => permit,
                    Err(_) => continue,
                };
                let rx = snapshot_tx.subscribe();
                let command_tx = command_tx.clone();
                tokio::spawn(serve_client(stream, rx, command_tx, permit));
            }
            // A transient accept error should not kill the server; yield and retry.
            Err(_) => {
                tokio::task::yield_now().await;
            }
        }
    }
}

/// Serve one UI client: push snapshots (current + every change) and read its commands.
async fn serve_client(
    stream: IpcServerStream,
    mut snapshot_rx: watch::Receiver<Snapshot>,
    command_tx: mpsc::Sender<Command>,
    _client_slot: OwnedSemaphorePermit,
) {
    let (mut read_half, mut write_half) = tokio::io::split(stream);

    // Push the snapshot the client sees on connect — best-effort. A client that only
    // sends a command and then immediately closes (the Connect/Disconnect/Trust path does
    // exactly this) may already be gone by the time we write here; that write fails, but it
    // must NOT make us return before draining the command it buffered on the read half
    // below — otherwise the command is silently lost (the cause of "disconnect does
    // nothing"). On a genuinely dead connection the read below errors and we return then.
    {
        let current = snapshot_rx.borrow_and_update().clone();
        let _ = write_message(&mut write_half, &ServerMessage::Snapshot(current)).await;
    }

    loop {
        tokio::select! {
            // `biased`: always try to drain a pending client command BEFORE writing a
            // snapshot. Otherwise a burst of snapshot publishes (the daemon republishes on
            // every discovery change) keeps the snapshot branch ready, and for a client
            // that sends a command then closes (Connect/Disconnect/Trust), the snapshot
            // write to the now-closed client fails and this task returns *before ever
            // reading the buffered command* — so the command is silently dropped. Reading
            // first guarantees the command is forwarded before any write can preempt it.
            biased;
            // The client sent a command — forward it to the daemon, replying to
            // GetSnapshot inline so the client need not wait for the next change.
            command = read_message::<_, Command>(&mut read_half) => {
                match command {
                    Ok(Command::GetSnapshot) => {
                        let snapshot = snapshot_rx.borrow_and_update().clone();
                        if write_message(&mut write_half, &ServerMessage::Snapshot(snapshot))
                            .await
                            .is_err()
                        {
                            return;
                        }
                    }
                    Ok(other) => {
                        if command_tx.send(other).await.is_err() {
                            return; // daemon gone
                        }
                    }
                    // Client closed or sent garbage — drop this connection.
                    Err(_) => return,
                }
            }
            // The daemon published a newer snapshot — forward it.
            changed = snapshot_rx.changed() => {
                if changed.is_err() {
                    return; // server dropped
                }
                let snapshot = snapshot_rx.borrow_and_update().clone();
                if write_message(&mut write_half, &ServerMessage::Snapshot(snapshot))
                    .await
                    .is_err()
                {
                    return;
                }
            }
        }
    }
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn verify_peer_uid(stream: &UnixStream) -> Result<(), std::io::Error> {
    use nix::sys::socket::{getsockopt, sockopt::PeerCredentials};
    use nix::unistd::Uid;

    let peer = getsockopt(stream, PeerCredentials).map_err(std::io::Error::from)?;
    verify_uid(peer.uid(), Uid::current().as_raw())
}

#[cfg(any(
    target_os = "macos",
    target_os = "ios",
    target_os = "freebsd",
    target_os = "dragonfly",
    target_os = "netbsd",
    target_os = "openbsd"
))]
fn verify_peer_uid(stream: &UnixStream) -> Result<(), std::io::Error> {
    use nix::sys::socket::{getsockopt, sockopt::LocalPeerCred};
    use nix::unistd::Uid;

    let peer = getsockopt(stream, LocalPeerCred).map_err(std::io::Error::from)?;
    verify_uid(peer.uid(), Uid::current().as_raw())
}

#[cfg(all(
    unix,
    not(any(
        target_os = "linux",
        target_os = "android",
        target_os = "macos",
        target_os = "ios",
        target_os = "freebsd",
        target_os = "dragonfly",
        target_os = "netbsd",
        target_os = "openbsd"
    ))
))]
fn verify_peer_uid(_stream: &UnixStream) -> Result<(), std::io::Error> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "ipc peer credential check is unsupported on this Unix target",
    ))
}

#[cfg(unix)]
fn verify_uid(peer_uid: u32, daemon_uid: u32) -> Result<(), std::io::Error> {
    if peer_uid == daemon_uid {
        return Ok(());
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::PermissionDenied,
        "ipc peer uid does not match daemon uid",
    ))
}
