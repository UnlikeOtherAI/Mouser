// Shared UI domain types for the Mouser desktop shell.
//
// These mirror the *shape* of what `mouser-ipc` will eventually deliver, but
// are intentionally UI-local for now: this pass has NO backend wiring
// (docs/architecture.md §8 — the Tauri UI links `mouser-ipc`, not core).

export type OsKind = "macos" | "windows" | "linux" | "phone";

export type ConnectionState = "connected" | "connecting" | "offline";

export type DeviceRole = "coordinator" | "member";

/**
 * A physical monitor attached to a device, in **logical points** in the global
 * desktop space — the same coordinate system the OS uses to arrange displays.
 * The layout canvas fits these to its viewport, so positions may be negative and
 * screens of different DPI line up 1:1. `x`/`y` is the top-left corner.
 */
export interface Monitor {
  id: string;
  width: number;
  height: number;
  x: number;
  y: number;
  /** Backing scale factor (2 on Retina); informational. */
  scale?: number;
}

/** A connected machine in the workspace. */
export interface Device {
  id: string;
  name: string;
  os: OsKind;
  state: ConnectionState;
  role: DeviceRole;
  monitors: Monitor[];
}

/**
 * A peer the **engine** has discovered on the LAN, delivered over `mouser-ipc`
 * (mirrors the backend `EnginePeer` / `mouser_ipc::PeerDto`). Unlike the old
 * UI-side mDNS shortcut, this carries the engine's trust decision, so the UI can
 * offer a real "Connect" action for trusted peers.
 */
export interface Peer {
  id: string;
  name: string;
  os: OsKind;
  /** First resolved IP address of the peer. */
  host: string;
  /** Interactive-connection UDP port (`iport`). */
  port: number;
  /** Whether this peer is user-approved (trusted) on this machine. */
  trusted: boolean;
}

/** Lifecycle of the engine's single peer connection (mirrors `ConnectionDto`). */
export type EngineConnectionState = "idle" | "connecting" | "connected";

/**
 * The engine's current connection/ownership state, delivered over `mouser-ipc`.
 */
export interface EngineConnection {
  state: EngineConnectionState;
  /** The peer being connected to (base32 id), when connecting/connected. */
  peerId: string | null;
  /** The device that currently owns input (base32 id), when connected. */
  owner: string | null;
  /** Current ownership epoch, when connected. */
  epoch: number | null;
  /** Why the last connection attempt failed, when known. */
  error: string | null;
}

/** Edge transfer behaviour (mirrors `SettingsDto.edge_behavior`). */
export type EdgeBehavior = "instant" | "delayed" | "locked";

/**
 * Daemon-owned settings, mirroring `mouser_ipc::SettingsDto` (snake_case to match
 * the wire). The engine is the source of truth; the UI reads these from the
 * snapshot and writes them with `updateSettings`, so buttons and the MCP server
 * drive the same state.
 */
export interface EngineSettings {
  // Pointer crossing (Input)
  cross_at_edges: boolean;
  edge_behavior: EdgeBehavior;
  wrap_around: boolean;
  share_scroll: boolean;
  // Clipboard (§7.7)
  shared_clipboard: boolean;
  clipboard_direction: SyncDirection;
  sync_text: boolean;
  sync_images: boolean;
  sync_files: boolean;
  max_auto_sync_bytes: number;
  prefer_native_apple: boolean;
  // Security
  require_approval: boolean;
  encrypted_only: boolean;
  release_on_lock: boolean;
  // General (application preferences)
  show_tray_icon: boolean;
  launch_at_login: boolean;
  theme: "system" | "light" | "dark";
  auto_update: boolean;
}

/** Defaults matching `SettingsDto::default()` (used as the browser-dev fallback). */
export const DEFAULT_ENGINE_SETTINGS: EngineSettings = {
  cross_at_edges: true,
  edge_behavior: "instant",
  wrap_around: false,
  share_scroll: true,
  shared_clipboard: true,
  clipboard_direction: "bidirectional",
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
  theme: "system",
  auto_update: true,
};

export type SectionId =
  | "general"
  | "devices"
  | "layout"
  | "input"
  | "clipboard"
  | "security"
  | "diagnostics";

export interface NavItem {
  id: SectionId;
  label: string;
}

// ---------------------------------------------------------------------------
// Clipboard (§7.7). These mirror `crates/mouser-clipboard/src/settings.rs`
// (`ClipboardSettings` / `SyncDirection`) and the engine's progress events
// (`reassembly::Progress`). UI-local for now: enforced in core once IPC lands.
// ---------------------------------------------------------------------------

/** Which way clipboard content may flow (Rust `SyncDirection`). */
export type SyncDirection = "bidirectional" | "send_only" | "receive_only";

/**
 * The Clipboard section of a device's settings (Rust `ClipboardSettings`).
 * Replicated per device, not cluster-wide.
 */
export interface ClipboardSettings {
  /** Master on/off. When false: no offer is sent and inbound offers ignored. */
  sharedClipboard: boolean;
  /** Per-format gate: utf8_text / html / rtf. */
  syncText: boolean;
  /** Per-format gate: png images. */
  syncImages: boolean;
  /** Per-format gate: uri_list (file references). */
  syncFiles: boolean;
  /** Skip eager auto-pull above this many bytes (0 = unlimited). */
  maxAutoSyncBytes: number;
  /** Prefer the OS Universal Clipboard between two Apple devices (default on). */
  preferNativeApple: boolean;
  /** Direction the clipboard may flow for this device. */
  direction: SyncDirection;
}

/** Clipboard payload kind, grouped to the three per-format gates (§7.7). */
export type ClipFormat = "text" | "image" | "files";

/** Direction of a single in-flight transfer relative to this device. */
export type TransferDirection = "incoming" | "outgoing";

/** Lifecycle of an in-flight transfer (engine `pending` → applied / dropped). */
export type TransferState = "active" | "done" | "failed";

/**
 * One in-flight clipboard transfer, mirroring the engine's progress events
 * (`reassembly::Progress`: `received_bytes` / `size`). Drives the Mac-style
 * "wait" indicator until `last = true` arrives and the hash verifies.
 */
export interface ClipboardTransfer {
  id: string;
  /** Display name of the peer device on the other end. */
  peer: string;
  direction: TransferDirection;
  format: ClipFormat;
  /** Contiguous bytes reassembled so far (`Progress.received_bytes`). */
  received: number;
  /** Total expected size from the offer (`Progress.size`). */
  total: number;
  state: TransferState;
}
