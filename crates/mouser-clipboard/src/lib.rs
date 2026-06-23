//! mouser-clipboard — the **clipboard sync engine** (docs/communication-interface.md §7.7).
//!
//! A pure, deterministic state machine ([`ClipboardEngine`]) that drives one device's
//! side of the §7.7 clipboard protocol over the message catalog
//! (`ClipboardOffer` → `ClipboardPull` → `ClipboardData`). Like [`mouser-files`] it is
//! I/O-free except through narrow seams, so the logic is unit-testable with **no OS
//! clipboard calls** and reusable unchanged by `mouser-engine`:
//!
//! - [`source::ClipContentSource`] — answers an inbound `ClipboardPull` with the
//!   canonical bytes of a representation this device offered.
//! - [`source::LocalRepr`] in / [`source::AppliedClip`] out — the caller hands the
//!   engine the local clipboard's raw representations to offer, and the engine hands
//!   back hash-verified content to write to the OS clipboard.
//!
//! ## What the engine enforces (§7.7)
//! - **Canonical hashing** ([`canonical`]): `hash = SHA-256(canonical(format, bytes))`
//!   so both ends agree on the pull/dedup/integrity key (text CRLF→LF + no trailing
//!   NUL; html/rtf/png as-is; uri_list LF-separated no trailing blank).
//! - **Eager auto-pull**: an inbound offer is answered immediately with a pull for the
//!   best accepted representation (`png` > `rtf` > `html` > `uri_list` > `utf8_text`),
//!   so content is in flight before the user pastes.
//! - **Prefer-native between Apple devices**: when both ends are `macos`/`ios` and the
//!   setting is on, Mouser suppresses its own sync and lets the OS Universal Clipboard
//!   carry it ([`settings::prefer_native_suppresses`]) — per peer pair, not global.
//! - **Transport by size**: a small text payload rides one control-stream message; a
//!   `png`/over-cap payload is split into ≤ 1 MiB ordered bulk chunks.
//! - **Reassembly + progress + verify** ([`reassembly`]): inbound chunks accumulate by
//!   `(origin, format, hash)`, expose [`reassembly::Progress`] for the "Pasting from
//!   <device>…" indicator, and the reassembled SHA-256 is verified against the offer's
//!   hash — dropped on mismatch, never applied.
//! - **Loop prevention**: a locally-applied `(origin, format, hash)` is recorded in a
//!   bounded log and suppresses the local clipboard-change echo once.
//! - **Settings gates** ([`settings`]): the master switch, per-format gates, auto-sync
//!   size limit, and direction are enforced **on send** *and* **on receipt**.
//!
//! [`mouser-files`]: https://docs.rs/mouser-files
//!
//! The runtime path is panic-free: `[workspace.lints.clippy]` denies
//! `unwrap_used`/`panic`/`indexing_slicing` crate-wide.

pub mod canonical;
pub mod engine;
pub mod reassembly;
pub mod settings;
pub mod source;
mod tracking;

pub use canonical::{canonical, content_hash, sha256};
pub use engine::{transport_for, ClipboardEngine, Transport};
pub use reassembly::{Progress, Reassembly};
pub use settings::{is_apple, prefer_native_suppresses, ClipboardSettings, SyncDirection};
pub use source::{AppliedClip, ClipContentSource, LocalRepr, MemContentSource};
pub use tracking::{MAX_APPLIED_CLIPS, MAX_PENDING_PULLS, PULL_STALL_TICKS};

/// A 32-byte SHA-256 digest: the canonical-content hash that is an offer/pull's
/// `hash` field, the dedup key, and the integrity check (§7.7).
pub type Hash = [u8; 32];

/// Hard cap on a single `ClipboardData.data` length (§0.3, bulk-stream): 1 MiB.
/// Larger payloads are chunked; an inbound chunk over this is rejected before commit.
pub const MAX_DATA_CHUNK: usize = 1024 * 1024;

/// Payloads at or below this size ride the **interactive control stream** as a single
/// `ClipboardData` (§7.7 "text formats within the control-stream cap"); larger ones go
/// over the bulk plane as ≤ [`MAX_DATA_CHUNK`] chunks. Set to the §0.3 control-stream
/// per-field cap (64 KiB) — the binding limit for the single-message decision.
pub const CONTROL_TEXT_CAP: usize = 64 * 1024;

/// Errors surfaced by the clipboard engine. Every variant is a *protocol/policy*
/// failure; none panic (the I/O seams are infallible value lookups). Policy *gates*
/// (settings off, prefer-native, size limit) are **not** errors — they return
/// `None`/empty so the caller simply emits nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClipboardError {
    /// A `ClipboardData` chunk exceeded [`MAX_DATA_CHUNK`] (§0.3) — rejected before
    /// allocation/commit.
    ChunkTooLarge(usize),
    /// A chunk's `offset`/end disagreed with the contiguous reassembly position or ran
    /// past the offered `size`.
    OffsetOutOfRange {
        /// The offset/length the accumulator expected.
        expected: u64,
        /// The offset/length the chunk actually carried.
        got: u64,
    },
    /// The reassembled payload's SHA-256 did not match the offered `hash` — the payload
    /// is dropped, never applied (§7.7).
    HashMismatch,
    /// A `ClipboardData` arrived for a `hash` with no in-flight pull.
    UnknownHash,
    /// A malformed message (wrong-length `hash`/`origin`, a format mismatch, a chunk
    /// after `last`, …).
    Protocol(String),
}

impl core::fmt::Display for ClipboardError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::ChunkTooLarge(n) => write!(f, "clipboard chunk {n} bytes exceeds 1 MiB cap"),
            Self::OffsetOutOfRange { expected, got } => {
                write!(
                    f,
                    "clipboard chunk offset/len {got} out of range (expected {expected})"
                )
            }
            Self::HashMismatch => f.write_str("reassembled clipboard hash mismatch (dropped)"),
            Self::UnknownHash => f.write_str("clipboard data for an unknown/unpulled hash"),
            Self::Protocol(m) => write!(f, "clipboard protocol: {m}"),
        }
    }
}

impl std::error::Error for ClipboardError {}
