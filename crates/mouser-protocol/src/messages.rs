//! Control-plane message types and their envelope `type` discriminants (§7).
//! Only the messages needed for the current milestone(s) are defined here; the rest
//! land as the engine grows, each as an additive `type` (§2 forward-compatibility).

use serde::{Deserialize, Serialize};

use crate::enums::{
    AckStatus, BlockedReason, CapState, CapabilitySet, ClipFormat, FocusKind, GoodbyeReason, Os,
    PointerMode, Role, ScrollUnit, TransferReason,
};

/// `[01] Hello` envelope type (§7.1).
pub const TYPE_HELLO: u16 = 0x01;
/// `[02] HelloAck` envelope type.
pub const TYPE_HELLO_ACK: u16 = 0x02;
/// `[03] PairingResult` envelope type (§7.1, §5).
pub const TYPE_PAIRING_RESULT: u16 = 0x03;
/// `[04] BulkHello` envelope type (§7.1, §5 step 5).
pub const TYPE_BULK_HELLO: u16 = 0x04;
/// `[05] Ping` envelope type.
pub const TYPE_PING: u16 = 0x05;
/// `[06] Pong` envelope type (§7.1).
pub const TYPE_PONG: u16 = 0x06;
/// `[07] Heartbeat` envelope type (§7.1).
pub const TYPE_HEARTBEAT: u16 = 0x07;
/// `[08] Goodbye` envelope type (§7.1).
pub const TYPE_GOODBYE: u16 = 0x08;
/// `[F0] DeviceName` — an extension control message whose payload is the dialing
/// device's display name as UTF-8 (no CBOR envelope). A controller sends it right after
/// connecting so the target can name the device in its pairing-approval prompt; it is
/// advisory (trust is still the §3 cert pin), and unknown to the sans-IO core (skipped
/// as a forward-compat type), so only the accept/pairing path reads it.
pub const TYPE_DEVICE_NAME: u16 = 0x00F0;

/// `[30] OwnershipTransfer` envelope type (§7.4).
pub const TYPE_OWNERSHIP_TRANSFER: u16 = 0x30;
/// `[31] OwnershipAck` envelope type (§7.4).
pub const TYPE_OWNERSHIP_ACK: u16 = 0x31;
/// `[32] FocusState` envelope type (§7.4).
pub const TYPE_FOCUS_STATE: u16 = 0x32;
/// `[33] CapabilityState` envelope type (§7.4).
pub const TYPE_CAPABILITY_STATE: u16 = 0x33;
/// `[34] OwnershipRequest` envelope type (§7.4).
pub const TYPE_OWNERSHIP_REQUEST: u16 = 0x34;
/// `[35] PointerModeReq` envelope type (§7.4/§7.6).
pub const TYPE_POINTER_MODE_REQ: u16 = 0x35;

/// `[40] KeyEvent` envelope type (§7.5).
pub const TYPE_KEY_EVENT: u16 = 0x40;
/// `[41] PointerButton` envelope type (§7.5).
pub const TYPE_POINTER_BUTTON: u16 = 0x41;
/// `[42] Scroll` envelope type (§7.5).
pub const TYPE_SCROLL: u16 = 0x42;

/// `[50] ClipboardOffer` envelope type (§7.7).
pub const TYPE_CLIPBOARD_OFFER: u16 = 0x50;
/// `[51] ClipboardPull` envelope type (§7.7).
pub const TYPE_CLIPBOARD_PULL: u16 = 0x51;
/// `[52] ClipboardData` envelope type (§7.7).
pub const TYPE_CLIPBOARD_DATA: u16 = 0x52;

/// `[60] FileOffer` envelope type (§7.8).
pub const TYPE_FILE_OFFER: u16 = 0x60;
/// `[61] FileAccept` envelope type (§7.8).
pub const TYPE_FILE_ACCEPT: u16 = 0x61;
/// `[62] FileReject` envelope type (§7.8).
pub const TYPE_FILE_REJECT: u16 = 0x62;
/// `[63] FileChunk` envelope type (§7.8).
pub const TYPE_FILE_CHUNK: u16 = 0x63;
/// `[64] FileAck` envelope type (§7.8).
pub const TYPE_FILE_ACK: u16 = 0x64;
/// `[65] FileDone` envelope type (§7.8).
pub const TYPE_FILE_DONE: u16 = 0x65;

/// `[04] BulkHello { device_id: bytes32, interactive_session_id: u64, channel_sig: bytes }`
/// (§7.1). The first frame on a bulk connection (§6.2): it binds the bulk plane to the
/// interactive session via `interactive_session_id` and proves identity with a
/// `channel_sig` over the bulk TLS exporter (§5 step 5). `device_id`/`channel_sig`
/// encode as CBOR byte strings (§0.1).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BulkHello {
    #[serde(with = "serde_bytes")]
    pub device_id: Vec<u8>,
    pub interactive_session_id: u64,
    #[serde(with = "serde_bytes")]
    pub channel_sig: Vec<u8>,
}

/// `[05] Ping { id: u64 }` — liveness probe; `Pong` echoes `id` for same-clock RTT (§7.1).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ping {
    pub id: u64,
}

/// `[02] HelloAck { status: AckStatus, reason?: str }` (§7.1) — the response to a
/// `Hello`: `status` accepts/rejects/defers the session; `reason` is an optional
/// human-readable note (present on reject/pending). `reason` is omitted when `None`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HelloAck {
    pub status: AckStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

// ---------------------------------------------------------------------------
// §7.1 Session & liveness
// ---------------------------------------------------------------------------

/// `[01] Hello { device_id, name, os, engine_version, capabilities, role, session_id,
/// channel_sig }` (§7.1) — the first control-stream message: announces identity and
/// capabilities and proves the session binding with `channel_sig` over the interactive
/// TLS exporter (§5 step 4). `device_id`/`channel_sig` encode as CBOR byte strings (§0.1).
/// Fields are declared in spec order so the CBOR map keys are byte-canonical.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hello {
    #[serde(with = "serde_bytes")]
    pub device_id: Vec<u8>,
    pub name: String,
    pub os: Os,
    pub engine_version: String,
    pub capabilities: CapabilitySet,
    pub role: Role,
    pub session_id: u64,
    #[serde(with = "serde_bytes")]
    pub channel_sig: Vec<u8>,
}

/// `[03] PairingResult { accepted: bool, reason?: str }` (§7.1, §5) — sent after the SAS
/// comparison: `accepted` reflects the user's approval. `reason` is omitted when `None`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairingResult {
    pub accepted: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// `[06] Pong { id: u64 }` (§7.1) — echoes [`Ping::id`] for a same-clock RTT sample.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pong {
    pub id: u64,
}

/// `[07] Heartbeat { seq: u64 }` (§7.1) — sent every ~1s; a peer is `Disconnected`
/// after 3 consecutive misses (the §7.4 reclaim trigger).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Heartbeat {
    pub seq: u64,
}

/// `[08] Goodbye { reason: GoodbyeReason }` (§7.1) — a graceful leave; an owner's
/// Goodbye triggers a handoff before the connection drops.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Goodbye {
    pub reason: GoodbyeReason,
}

// ---------------------------------------------------------------------------
// §7.4 Ownership, focus & capability
// ---------------------------------------------------------------------------

/// `[30] OwnershipTransfer { to, owner_epoch, layout_rev, reason }` (§7.4) — an
/// owner-minted grant of input ownership to `to` at `owner_epoch`. Accepted iff
/// `owner_epoch` is strictly greater than the locally-known epoch (§7.4). `to` encodes
/// as a CBOR byte string (§0.1).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OwnershipTransfer {
    #[serde(with = "serde_bytes")]
    pub to: Vec<u8>,
    pub owner_epoch: u64,
    pub layout_rev: u64,
    pub reason: TransferReason,
}

/// `[31] OwnershipAck { owner_epoch, accepted, reason? }` (§7.4) — the target's
/// acknowledgement of an [`OwnershipTransfer`]: `accepted` reports willingness to
/// inject. `reason` is omitted when `None`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OwnershipAck {
    pub owner_epoch: u64,
    pub accepted: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// `[32] FocusState { owner, owner_epoch, state }` (§7.4) — broadcast when the owner or
/// focus changes; subject to the same strictly-greater-epoch acceptance rule as
/// [`OwnershipTransfer`]. `owner` encodes as a CBOR byte string (§0.1).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FocusState {
    #[serde(with = "serde_bytes")]
    pub owner: Vec<u8>,
    pub owner_epoch: u64,
    pub state: FocusKind,
}

/// `[33] CapabilityState { device_id, capture, inject, reason }` (§7.4) — broadcast when
/// input capture/injection becomes (un)available (secure desktop, lock screen, missing
/// permission, unsupported compositor). `device_id` is a CBOR byte string (§0.1).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapabilityState {
    #[serde(with = "serde_bytes")]
    pub device_id: Vec<u8>,
    pub capture: CapState,
    pub inject: CapState,
    pub reason: BlockedReason,
}

/// `[34] OwnershipRequest { from, reason }` (§7.4) — a non-owner asks the current owner
/// to hand off (e.g. mobile `UiSelect`). `from` encodes as a CBOR byte string (§0.1).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OwnershipRequest {
    #[serde(with = "serde_bytes")]
    pub from: Vec<u8>,
    pub reason: TransferReason,
}

/// `[35] PointerModeReq { owner_epoch, mode }` (§7.4/§7.6) — the target asks the owner
/// to switch absolute/relative motion (relative on a foreground pointer-lock grab).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PointerModeReq {
    pub owner_epoch: u64,
    pub mode: PointerMode,
}

// ---------------------------------------------------------------------------
// §7.5 Input — reliable (control stream)
// ---------------------------------------------------------------------------

/// `[40] KeyEvent { usage, down, mods, owner_epoch, ctr }` (§7.5) — a physical key
/// transition. `usage` = USB HID Usage Page 0x07 (Appendix B); `mods` is the modifier
/// bitmask. Anti-replay: a receiver rejects an event whose `owner_epoch` is not current
/// or whose `(owner_epoch, ctr)` is not strictly increasing (§7.5).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeyEvent {
    pub usage: u16,
    pub down: bool,
    pub mods: u16,
    pub owner_epoch: u64,
    pub ctr: u64,
}

/// `[41] PointerButton { button, down, owner_epoch, ctr }` (§7.5) — a mouse-button
/// transition (`button`: 0=left, 1=right, 2=middle, 3=back, 4=forward). Same anti-replay
/// rule as [`KeyEvent`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PointerButton {
    pub button: u8,
    pub down: bool,
    pub owner_epoch: u64,
    pub ctr: u64,
}

/// `[42] Scroll { dx, dy, unit, owner_epoch, ctr }` (§7.5) — a scroll delta in `unit`
/// (`Detent120` or `LogicalPixel`); the receiver converts to its native unit. Same
/// anti-replay rule as [`KeyEvent`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Scroll {
    pub dx: i32,
    pub dy: i32,
    pub unit: ScrollUnit,
    pub owner_epoch: u64,
    pub ctr: u64,
}

/// One advertised file inside a [`FileOffer`] (§7.8): a sanitized-on-receipt `name`
/// and the total `size` in bytes (bounds the transfer; the receiver pre-allocates
/// nothing beyond a single chunk).
///
/// `sha256` is the optional 32-byte SHA-256 of the file's full contents (audit R2
/// **C2-4**): when present it travels in-band so the receiver can verify integrity
/// end-to-end (compare against the reassembled digest before committing `ok=true`).
/// It encodes as a CBOR **byte string** (§0.1) and is **omitted entirely** when
/// `None`, so an offer without it is byte-identical to the pre-`sha256` wire form.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileEntry {
    pub name: String,
    pub size: u64,
    #[serde(default, skip_serializing_if = "Option::is_none", with = "serde_bytes")]
    pub sha256: Option<Vec<u8>>,
}

/// `[60] FileOffer { transfer_id, files: [{ name, size }] }` (§7.8). Sent on the
/// bulk connection's dedicated per-`transfer_id` stream to begin a transfer.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileOffer {
    pub transfer_id: u64,
    pub files: Vec<FileEntry>,
}

/// A per-file resume point inside a [`FileAccept`] (§7.8): the receiver already holds
/// `offset` valid bytes of `file_index`, so the sender starts there.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResumePoint {
    pub file_index: u32,
    pub offset: u64,
}

/// `[61] FileAccept { transfer_id, resume: [{ file_index, offset }] }` (§7.8). An
/// empty `resume` means "start every file at offset 0".
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileAccept {
    pub transfer_id: u64,
    pub resume: Vec<ResumePoint>,
}

/// `[62] FileReject { transfer_id, reason }` (§7.8).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileReject {
    pub transfer_id: u64,
    pub reason: String,
}

/// `[63] FileChunk { transfer_id, file_index, offset, data }` (§7.8). `data` encodes
/// as a CBOR **byte string** (§0.1) via `serde_bytes`; `data.len()` ≤ 1 MiB per frame
/// (§0.3) — chunk larger payloads.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileChunk {
    pub transfer_id: u64,
    pub file_index: u32,
    pub offset: u64,
    #[serde(with = "serde_bytes")]
    pub data: Vec<u8>,
}

/// `[64] FileAck { transfer_id, file_index, acked_through }` (§7.8). `acked_through`
/// is the **cumulative** number of contiguous bytes the receiver has committed for
/// `file_index`; the sender keeps ≤ 8 MiB in flight beyond it per file (§0.3).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileAck {
    pub transfer_id: u64,
    pub file_index: u32,
    pub acked_through: u64,
}

/// `[65] FileDone { transfer_id, ok }` (§7.8). `ok = false` signals the sender or
/// receiver aborted (integrity failure, oversize, cancel).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileDone {
    pub transfer_id: u64,
    pub ok: bool,
}

/// One advertised clipboard representation inside a [`ClipboardOffer`] (§7.7): a
/// `format`, the content `hash` (`SHA-256(canonical(format, bytes))`, the pull/dedup
/// key and integrity check), and the total `size` in bytes (bounds the transfer and
/// drives the progress indicator). `hash` encodes as a CBOR byte string (§0.1).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClipboardEntry {
    pub format: ClipFormat,
    #[serde(with = "serde_bytes")]
    pub hash: Vec<u8>,
    pub size: u64,
}

/// `[50] ClipboardOffer { entries, origin }` (§7.7). Broadcast on the control stream
/// when the local clipboard changes: it advertises every available representation so a
/// peer can pull the one it wants. `origin` is the offering device's `device_id`; a
/// locally-applied `(origin, hash)` is not re-offered (loop prevention). A new offer
/// supersedes outstanding offers from the same origin. `origin` is a CBOR byte string.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClipboardOffer {
    pub entries: Vec<ClipboardEntry>,
    #[serde(with = "serde_bytes")]
    pub origin: Vec<u8>,
}

/// `[51] ClipboardPull { hash, format }` (§7.7). A peer requests the bytes of one
/// offered representation (by `hash`+`format`). With immediate-sync enabled, receivers
/// auto-pull on offer so content is already in flight before the user pastes; the
/// `hash` is a CBOR byte string.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClipboardPull {
    #[serde(with = "serde_bytes")]
    pub hash: Vec<u8>,
    pub format: ClipFormat,
}

/// `[52] ClipboardData { hash, format, offset, data, last }` (§7.7). Carries clipboard
/// bytes for a pulled `hash`. Small text formats ride the interactive control stream as
/// a single message (`offset = 0`, `last = true`); `png`/over-cap payloads ride the bulk
/// connection as ordered chunks (`data` ≤ 1 MiB, `offset` = byte offset, `last` on the
/// final chunk). The puller verifies the reassembled bytes against `hash`. `hash`/`data`
/// encode as CBOR byte strings (§0.1).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClipboardData {
    #[serde(with = "serde_bytes")]
    pub hash: Vec<u8>,
    pub format: ClipFormat,
    pub offset: u64,
    #[serde(with = "serde_bytes")]
    pub data: Vec<u8>,
    pub last: bool,
}
