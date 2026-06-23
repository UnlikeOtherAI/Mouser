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
    /// The reason the last connection attempt failed, when known. Set when a dial
    /// requested over IPC could not complete (e.g. the peer is not trusted, not
    /// discoverable, or unreachable) so the UI can explain the failure instead of
    /// silently returning to idle. Cleared on a new attempt or a successful connect.
    #[serde(default)]
    pub error: Option<String>,
}

impl Default for ConnectionDto {
    fn default() -> Self {
        Self {
            state: ConnectionStateDto::Idle,
            peer_id: None,
            owner: None,
            epoch: None,
            error: None,
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
    /// A pending inbound pairing request awaiting the user's approval, if any. Set when
    /// an untrusted peer dials this machine; the UI shows an Allow/Deny prompt naming the
    /// device, and answers with [`Command::ApprovePairing`]/[`Command::DenyPairing`].
    #[serde(default)]
    pub pairing: Option<PairingDto>,
    /// Daemon-owned, persisted settings (input/clipboard/security). The UI and the MCP
    /// server both read these here and write them via [`Command::UpdateSettings`].
    #[serde(default)]
    pub settings: SettingsDto,
    /// Connectivity/permission health the daemon detected (empty = healthy). Surfaced in
    /// the UI and over MCP so problems (discovery leaving via a dead adapter, a firewall
    /// blocking inbound, advertising-but-no-peers, a missing OS permission) are explained
    /// with an optional one-click fix, instead of a silent "no devices found".
    #[serde(default)]
    pub diagnostics: Vec<HealthItemDto>,
}

/// Severity of a [`HealthItemDto`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HealthSeverity {
    /// Informational — discovery works, but something is worth noting.
    #[default]
    Info,
    /// Likely degrades discovery/connection, but not necessarily fatal.
    Warning,
    /// Prevents discovery/connection until resolved.
    Error,
}

impl HealthSeverity {
    /// The lowercase wire/UI label (`"info" | "warning" | "error"`).
    pub fn as_str(self) -> &'static str {
        match self {
            HealthSeverity::Info => "info",
            HealthSeverity::Warning => "warning",
            HealthSeverity::Error => "error",
        }
    }
}

/// One connectivity/permission health finding the daemon detected (spec §9). Surfaced in
/// the [`Snapshot`] so the UI and the MCP server can explain *why* discovery/connection
/// is failing and offer a one-click fix. Forward compatible: optional fields default.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HealthItemDto {
    /// Stable machine code, e.g. `"advertising_zero_peers"` or `"mdns_dead_egress"` —
    /// the UI/MCP key off this, the human text may change.
    pub code: String,
    /// How serious it is.
    #[serde(default)]
    pub severity: HealthSeverity,
    /// Short human-readable title.
    pub title: String,
    /// Concrete detail, including the offending value (adapter name, address, …) so the
    /// explanation is specific.
    pub detail: String,
    /// Optional remediation action id the UI/MCP can trigger (e.g.
    /// `"open_network_settings"`, `"add_firewall_rule"`). `None` = nothing to auto-fix.
    #[serde(default)]
    pub remediation: Option<String>,
}

/// An inbound pairing request from an untrusted peer that dialed this machine: the peer's
/// base32 device id and the display name it announced (advisory — trust is still the §3
/// cert pin keyed on `peer_id`; the name just lets the user recognize the device).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairingDto {
    /// Base32 device id of the peer requesting control.
    pub peer_id: String,
    /// The peer's announced display name (e.g. "Ondrej's iPhone"), or a generic fallback.
    pub name: String,
}

/// Daemon-owned, persisted settings — the single source of truth that both the
/// desktop UI and the MCP server read (from the snapshot) and write (via
/// [`Command::UpdateSettings`]), replacing per-surface local toggles so the same
/// state is editable from buttons and programmatically (spec §7.5–§7.7, §9).
///
/// Every field carries a serde default so older/newer daemons stay forward
/// compatible. Persistence + exposure is daemon-owned; full *enforcement* in the
/// engine/clipboard paths is incremental.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SettingsDto {
    // --- Pointer crossing (Input) ---
    /// Cross to an adjacent device when the cursor reaches a shared edge.
    #[serde(default = "yes")]
    pub cross_at_edges: bool,
    /// Edge transfer behaviour: `"instant" | "delayed" | "locked"`.
    #[serde(default = "edge_default")]
    pub edge_behavior: String,
    /// Crossing the far edge returns the cursor to the opposite side.
    #[serde(default)]
    pub wrap_around: bool,
    /// Forward scroll-wheel events to the device that owns the cursor.
    #[serde(default = "yes")]
    pub share_scroll: bool,

    // --- Clipboard (§7.7) ---
    /// Master clipboard switch. Off ⇒ nothing sent and inbound copies ignored.
    #[serde(default = "yes")]
    pub shared_clipboard: bool,
    /// Direction: `"bidirectional" | "send_only" | "receive_only"`.
    #[serde(default = "direction_default")]
    pub clipboard_direction: String,
    /// Sync text (plain/HTML/RTF).
    #[serde(default = "yes")]
    pub sync_text: bool,
    /// Sync images (PNG).
    #[serde(default = "yes")]
    pub sync_images: bool,
    /// Sync file references (uri-list).
    #[serde(default = "yes")]
    pub sync_files: bool,
    /// Skip eager auto-pull above this many bytes (0 = unlimited).
    #[serde(default)]
    pub max_auto_sync_bytes: u64,
    /// Prefer the OS Universal Clipboard between two Apple devices.
    #[serde(default = "yes")]
    pub prefer_native_apple: bool,

    // --- Security ---
    /// Require an approval prompt (SAS) before a new device may send input.
    #[serde(default = "yes")]
    pub require_approval: bool,
    /// Refuse peers that fail certificate pinning (§3).
    #[serde(default = "yes")]
    pub encrypted_only: bool,
    /// Return input ownership to the local device on sleep/lock.
    #[serde(default = "yes")]
    pub release_on_lock: bool,

    // --- General (application preferences) ---
    /// Keep the menu-bar/system-tray icon visible.
    #[serde(default = "yes")]
    pub show_tray_icon: bool,
    /// Start Mouser automatically when the user signs in (OS autostart).
    #[serde(default)]
    pub launch_at_login: bool,
    /// UI theme choice: `"system" | "light" | "dark"`.
    #[serde(default = "theme_default")]
    pub theme: String,
    /// Download and install new versions automatically.
    #[serde(default = "yes")]
    pub auto_update: bool,
}

fn yes() -> bool {
    true
}
fn edge_default() -> String {
    "instant".to_string()
}
fn direction_default() -> String {
    "bidirectional".to_string()
}
fn theme_default() -> String {
    "system".to_string()
}

impl Default for SettingsDto {
    fn default() -> Self {
        Self {
            cross_at_edges: true,
            edge_behavior: edge_default(),
            wrap_around: false,
            share_scroll: true,
            shared_clipboard: true,
            clipboard_direction: direction_default(),
            sync_text: true,
            sync_images: true,
            sync_files: true,
            max_auto_sync_bytes: 0,
            prefer_native_apple: true,
            require_approval: true,
            encrypted_only: true,
            release_on_lock: true,
            show_tray_icon: true,
            launch_at_login: false,
            theme: theme_default(),
            auto_update: true,
        }
    }
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
    /// Pair (trust) a discovered peer on this machine by base32 id, so the engine
    /// will allow a connection to/from it. The daemon updates its trust store and
    /// republishes a fresh snapshot, so the UI reflects the new trust immediately
    /// (a separate `mouserd trust` process could not, since the running daemon serves
    /// a cached snapshot it only rebuilds on its own state changes).
    Trust {
        /// Base32 device id of the peer to trust.
        peer_id: String,
    },
    /// Approve a pending inbound pairing request (trust the peer + accept its connection).
    ApprovePairing {
        /// Base32 device id of the peer to approve (matches [`PairingDto::peer_id`]).
        peer_id: String,
    },
    /// Deny a pending inbound pairing request (close the connection, do not trust).
    DenyPairing {
        /// Base32 device id of the peer to deny.
        peer_id: String,
    },
    /// Replace the daemon's persisted settings (full struct). The daemon saves them
    /// and republishes a fresh snapshot so every connected surface (UI + MCP) reflects
    /// the change. Callers read the current settings from the snapshot, modify, send.
    UpdateSettings {
        /// The complete new settings.
        settings: SettingsDto,
    },
    /// Ask the daemon to reply with the current [`Snapshot`] immediately.
    GetSnapshot,
}
