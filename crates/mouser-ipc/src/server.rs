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

#[cfg(windows)]
use tokio::net::windows::named_pipe::{NamedPipeServer, ServerOptions};
#[cfg(unix)]
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{mpsc, watch};

use crate::codec::{read_message, write_message, IpcError};
use crate::dto::{Command, ServerMessage, Snapshot};
use crate::path::default_socket_path;

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
    command_rx: mpsc::UnboundedReceiver<Command>,
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
        match std::fs::remove_file(&socket_path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(IpcError::Io(e)),
        }
        let listener = IpcListener::bind(socket_path.clone())?;
        let (snapshot_tx, _snapshot_rx) = watch::channel(initial);
        let (command_tx, command_rx) = mpsc::unbounded_channel();

        let accept_task = tokio::spawn(accept_loop(listener, snapshot_tx.clone(), command_tx));

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
        Ok(Self {
            inner: UnixListener::bind(path)?,
        })
    }

    async fn accept(&mut self) -> Result<IpcServerStream, std::io::Error> {
        self.inner.accept().await.map(|(stream, _addr)| stream)
    }
}

#[cfg(windows)]
struct IpcListener {
    pipe_name: PathBuf,
    pending: NamedPipeServer,
}

#[cfg(windows)]
impl IpcListener {
    fn bind(pipe_name: PathBuf) -> Result<Self, IpcError> {
        let pending = ServerOptions::new()
            .first_pipe_instance(true)
            .create(pipe_name.as_os_str())?;
        Ok(Self { pipe_name, pending })
    }

    async fn accept(&mut self) -> Result<IpcServerStream, std::io::Error> {
        self.pending.connect().await?;
        let next = ServerOptions::new().create(self.pipe_name.as_os_str())?;
        Ok(std::mem::replace(&mut self.pending, next))
    }
}

/// Accept connections forever, spawning a per-client task for each.
async fn accept_loop(
    mut listener: IpcListener,
    snapshot_tx: watch::Sender<Snapshot>,
    command_tx: mpsc::UnboundedSender<Command>,
) {
    loop {
        match listener.accept().await {
            Ok(stream) => {
                let rx = snapshot_tx.subscribe();
                let command_tx = command_tx.clone();
                tokio::spawn(serve_client(stream, rx, command_tx));
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
    command_tx: mpsc::UnboundedSender<Command>,
) {
    let (mut read_half, mut write_half) = tokio::io::split(stream);

    // Push the snapshot the client sees on connect.
    {
        let current = snapshot_rx.borrow_and_update().clone();
        if write_message(&mut write_half, &ServerMessage::Snapshot(current))
            .await
            .is_err()
        {
            return;
        }
    }

    loop {
        tokio::select! {
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
                        if command_tx.send(other).is_err() {
                            return; // daemon gone
                        }
                    }
                    // Client closed or sent garbage — drop this connection.
                    Err(_) => return,
                }
            }
        }
    }
}
