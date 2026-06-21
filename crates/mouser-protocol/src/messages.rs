//! Control-plane message types and their envelope `type` discriminants (§7).
//! Only the messages needed for the current milestone(s) are defined here; the rest
//! land as the engine grows, each as an additive `type` (§2 forward-compatibility).

use serde::{Deserialize, Serialize};

/// `[02] HelloAck` envelope type.
pub const TYPE_HELLO_ACK: u16 = 0x02;
/// `[04] BulkHello` envelope type (§7.1, §5 step 5).
pub const TYPE_BULK_HELLO: u16 = 0x04;
/// `[05] Ping` envelope type.
pub const TYPE_PING: u16 = 0x05;

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
