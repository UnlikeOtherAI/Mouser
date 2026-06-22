//! The typed data-transfer objects exchanged over the local IPC link.
//!
//! These are the contract between the headless `mouserd` engine daemon (which owns
//! discovery, trust, and the live connection) and the Tauri desktop UI (which reflects
//! that state and drives it). Everything here is plain serde data: the daemon builds a
//! [`Snapshot`] from its discovered peers + `DaemonStore` trust + current connection,
//! pushes it to connected UIs, and the UI sends [`Command`]s back.
//!
//! The wire encoding is CBOR (see [`crate::codec`]); these structs are the schema.

use serde::{Deserialize, Serialize};

/// The local machine, as the engine knows it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceDto {
    /// Base32 device id (the engine's persistent identity), or empty if unknown.
    pub id: String,
    /// Friendly host/device name.
    pub name: String,
    /// OS kind: `"macos" | "windows" | "linux"`.
    pub os: String,
}

/// A peer the engine has discovered on the LAN (mDNS), with its trust status.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeerDto {
    /// Base32 device id from the peer's advertisement.
    pub id: String,
    /// Advertised friendly name.
    pub name: String,
    /// Advertised OS kind.
    pub os: String,
    /// First resolved IP address (empty if not yet resolved / not dialable).
    pub host: String,
    /// Interactive-connection UDP port (`iport`); `0` if not dialable.
    pub port: u16,
    /// Whether this peer is user-approved on this machine (`DaemonStore` pin).
    pub trusted: bool,
}

/// The lifecycle of the engine's single peer connection (v1 single-peer topology).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConnectionStateDto {
    /// No connection and none in progress.
    Idle,
    /// A connection is being established to `peer_id`.
    Connecting,
    /// Connected to `peer_id`.
    Connected,
}

impl ConnectionStateDto {
    /// The lowercase wire/UI label (`"idle" | "connecting" | "connected"`).
    pub fn as_str(self) -> &'static str {
        match self {
            ConnectionStateDto::Idle => "idle",
            ConnectionStateDto::Connecting => "connecting",
            ConnectionStateDto::Connected => "connected",
        }
    }
}

/// The engine's current connection/ownership state.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConnectionDto {
    /// Lifecycle state.
    pub state: ConnectionStateDto,
    /// The peer this connection is to (base32 id), when connecting/connected.
    pub peer_id: Option<String>,
    /// The base32 id of the device that currently owns input, when connected.
    pub owner: Option<String>,
    /// The current ownership epoch, when connected.
    pub epoch: Option<u64>,
}

impl Default for ConnectionDto {
    fn default() -> Self {
        Self {
            state: ConnectionStateDto::Idle,
            peer_id: None,
            owner: None,
            epoch: None,
        }
    }
}

/// A full picture of the engine's state, pushed to UIs on change and on request.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    /// This machine, as the engine sees it.
    pub local: DeviceDto,
    /// Peers discovered on the LAN, with trust status.
    pub peers: Vec<PeerDto>,
    /// The current peer connection state.
    pub connection: ConnectionDto,
}

/// A message the daemon sends to a connected UI client.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ServerMessage {
    /// A fresh engine snapshot (pushed on change and in reply to `GetSnapshot`).
    Snapshot(Snapshot),
}

/// A command a UI client sends to the daemon.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Command {
    /// Request the engine connect to a discovered, trusted peer by base32 id.
    Connect {
        /// Base32 device id of the peer to connect to.
        peer_id: String,
        /// Optional resolved host/IP supplied by a desktop-side mDNS browser.
        host: Option<String>,
        /// Optional interactive UDP port paired with `host`.
        port: Option<u16>,
    },
    /// Request the engine tear down the current connection.
    Disconnect,
    /// Ask the daemon to reply with the current [`Snapshot`] immediately.
    GetSnapshot,
}
