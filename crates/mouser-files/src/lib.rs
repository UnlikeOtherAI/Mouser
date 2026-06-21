//! mouser-files — the **file-transfer engine** (docs/communication-interface.md §7.8).
//!
//! Pure sender/receiver state machines that drive a transfer over the §7.8 message
//! catalog (`FileOffer` → `FileAccept`/`FileReject` → `FileChunk`/`FileAck` →
//! `FileDone`). They are I/O-free except through two narrow traits so the logic can be
//! unit-tested deterministically and reused unchanged by `mouser-engine` over the
//! bulk connection (§6.2):
//!
//! - [`FileSource`] — the **sender** reads bytes to transmit (a file, a `Vec`, …).
//! - [`FileSink`] — the **receiver** commits validated bytes (a quarantined file, a
//!   `Vec`, …). The receiver decides *where* via [`path`] sanitization before the
//!   first byte lands.
//!
//! ## What the engine enforces
//! - **Multi-file** transfers in one `transfer_id` (offer lists every file).
//! - **Cumulative-ack windowing**: the sender keeps ≤ [`MAX_IN_FLIGHT_PER_FILE`]
//!   (8 MiB, §0.3) unacked bytes in flight per file; the receiver acks the contiguous
//!   prefix it has committed.
//! - **Resume from offsets**: `FileAccept.resume` lets the sender skip bytes the
//!   receiver already holds; a re-offered transfer continues instead of restarting.
//! - **Per-file size from the offer** bounds every write — an out-of-range `offset`
//!   or a byte past `size` is rejected, never allocated for.
//! - **Integrity check on completion**: both sides roll a SHA-256; the receiver gates
//!   `FileDone.ok` on size + (when the caller supplies the expected digest) hash.
//! - **Path safety** ([`path`]): the offered `name` is sanitized (separators stripped,
//!   `..`/absolute/symlink rejected) and the file lands inside a caller-provided
//!   quarantine dir — never outside it (`../../.ssh/authorized_keys` → rejected).

#![deny(clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]

pub mod path;
pub mod receiver;
pub mod sender;
pub mod sink;

pub use path::{resolve_in_quarantine, sanitize_name, PathError};
pub use receiver::{FileState, Outbound, Receiver, ReceiverConfig};
pub use sender::Sender;
pub use sink::{sha256, FileSink, FileSource, MemSink, MemSource, SinkError};

/// Max unacked bytes in flight **per file** (§0.3 file-transfer window): 8 MiB.
pub const MAX_IN_FLIGHT_PER_FILE: u64 = 8 * 1024 * 1024;

/// Default `FileChunk.data` size the sender emits. ≤ 1 MiB per frame (§0.3); chunk
/// larger payloads. 256 KiB balances syscall/frame overhead against window latency.
pub const DEFAULT_CHUNK_SIZE: usize = 256 * 1024;

/// Hard cap on a single `FileChunk.data` length the receiver will accept (§0.3): 1 MiB.
/// Oversize chunks are rejected *before* allocating, per the decode discipline.
pub const MAX_CHUNK_SIZE: usize = 1024 * 1024;

/// A 32-byte SHA-256 digest used for the optional end-to-end integrity check.
pub type Hash = [u8; 32];

/// Errors surfaced by the transfer engine. Every variant is a *protocol/policy*
/// failure (the I/O traits surface their own [`SinkError`]); none panic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileError {
    /// A message referenced a `transfer_id` this state machine does not own.
    UnknownTransfer(u64),
    /// A message referenced a `file_index` outside the offered file list.
    UnknownFileIndex(u32),
    /// A `FileChunk` exceeded [`MAX_CHUNK_SIZE`] (§0.3) — rejected before allocation.
    ChunkTooLarge(usize),
    /// A chunk/ack `offset` was past the file's declared `size`, or otherwise invalid.
    OffsetOutOfRange { file_index: u32, offset: u64 },
    /// The reassembled file's SHA-256 did not match the expected digest (integrity).
    HashMismatch { file_index: u32 },
    /// The offer/accept itself was malformed (e.g. duplicate file index, empty list).
    Protocol(String),
    /// A path-safety violation while resolving the offered `name` (see [`PathError`]).
    Path(PathError),
    /// The backing [`FileSink`]/[`FileSource`] failed.
    Sink(SinkError),
}

impl core::fmt::Display for FileError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::UnknownTransfer(id) => write!(f, "unknown transfer_id {id}"),
            Self::UnknownFileIndex(i) => write!(f, "unknown file_index {i}"),
            Self::ChunkTooLarge(n) => write!(f, "chunk {n} bytes exceeds 1 MiB cap"),
            Self::OffsetOutOfRange { file_index, offset } => {
                write!(f, "offset {offset} out of range for file {file_index}")
            }
            Self::HashMismatch { file_index } => {
                write!(f, "integrity hash mismatch for file {file_index}")
            }
            Self::Protocol(m) => write!(f, "file protocol: {m}"),
            Self::Path(e) => write!(f, "path: {e}"),
            Self::Sink(e) => write!(f, "sink: {e}"),
        }
    }
}

impl std::error::Error for FileError {}

impl From<PathError> for FileError {
    fn from(e: PathError) -> Self {
        Self::Path(e)
    }
}

impl From<SinkError> for FileError {
    fn from(e: SinkError) -> Self {
        Self::Sink(e)
    }
}
