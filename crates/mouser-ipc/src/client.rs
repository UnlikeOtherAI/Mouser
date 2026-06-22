//! UI-side IPC: connect to the daemon socket, receive [`Snapshot`]s, and send
//! [`Command`]s.
//!
//! The desktop shell connects once and then either polls [`Client::next_snapshot`] (the
//! daemon pushes a fresh snapshot on connect and on every change) or sends a [`Command`]
//! with [`Client::send_command`]. If the daemon is not running, [`Client::connect`]
//! fails and the UI degrades gracefully (local device + "engine not running" hint).

use std::path::Path;

use tokio::io::{ReadHalf, WriteHalf};
use tokio::net::UnixStream;

use crate::codec::{read_message, write_message, IpcError};
use crate::dto::{Command, ServerMessage, Snapshot};
use crate::path::default_socket_path;

/// A connected IPC client (UI side).
pub struct Client {
    read_half: ReadHalf<UnixStream>,
    write_half: WriteHalf<UnixStream>,
}

impl Client {
    /// Connect to the daemon at the well-known socket path.
    pub async fn connect() -> Result<Self, IpcError> {
        Self::connect_at(default_socket_path()).await
    }

    /// Connect to the daemon at an explicit socket path (tests pass a temp path).
    pub async fn connect_at(socket_path: impl AsRef<Path>) -> Result<Self, IpcError> {
        let stream = UnixStream::connect(socket_path.as_ref()).await?;
        let (read_half, write_half) = tokio::io::split(stream);
        Ok(Self {
            read_half,
            write_half,
        })
    }

    /// Send a command to the daemon.
    pub async fn send_command(&mut self, command: &Command) -> Result<(), IpcError> {
        write_message(&mut self.write_half, command).await
    }

    /// Await the next message from the daemon, returning its snapshot. Resolves once
    /// per pushed snapshot (on connect, on change, and in reply to `GetSnapshot`).
    pub async fn next_snapshot(&mut self) -> Result<Snapshot, IpcError> {
        let ServerMessage::Snapshot(snapshot) = read_message(&mut self.read_half).await?;
        Ok(snapshot)
    }

    /// Request and await a single fresh snapshot (sends `GetSnapshot`, then reads).
    ///
    /// Convenience for the UI's poll path: one round-trip without a background reader.
    /// Note any snapshot the daemon pushed before the reply is returned first (the
    /// reply will arrive on a later `next_snapshot` call); the UI treats every snapshot
    /// as current, so this is harmless.
    pub async fn fetch_snapshot(&mut self) -> Result<Snapshot, IpcError> {
        self.send_command(&Command::GetSnapshot).await?;
        self.next_snapshot().await
    }
}
