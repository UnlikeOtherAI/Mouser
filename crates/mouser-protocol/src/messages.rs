//! Control-plane message types and their envelope `type` discriminants (§7).
//! Only the messages needed for the current milestone(s) are defined here; the rest
//! land as the engine grows, each as an additive `type` (§2 forward-compatibility).

use serde::{Deserialize, Serialize};

use crate::enums::{AckStatus, ClipFormat};

/// `[02] HelloAck` envelope type.
pub const TYPE_HELLO_ACK: u16 = 0x02;
/// `[04] BulkHello` envelope type (§7.1, §5 step 5).
pub const TYPE_BULK_HELLO: u16 = 0x04;
/// `[05] Ping` envelope type.
pub const TYPE_PING: u16 = 0x05;

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

/// One advertised file inside a [`FileOffer`] (§7.8): a sanitized-on-receipt `name`
/// and the total `size` in bytes (bounds the transfer; the receiver pre-allocates
/// nothing beyond a single chunk).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileEntry {
    pub name: String,
    pub size: u64,
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
