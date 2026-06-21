//! The **receiver** state machine (§7.8). Driven by inbound `FileOffer`/`FileChunk`
//! messages, it emits the `FileAccept`/`FileReject`/`FileAck`/`FileDone` replies and
//! commits validated bytes to a per-file [`FileSink`]. Pure: the only I/O is through
//! the sink factory and the sinks themselves.
//!
//! Safety properties enforced here (audit M3 + §0.3):
//! - Every offered `name` is resolved through [`crate::path`] **before** a sink is
//!   created; a traversal/unsafe name rejects the whole transfer (`FileReject`).
//! - A chunk past the file's declared `size`, or larger than [`crate::MAX_CHUNK_SIZE`]
//!   (1 MiB), is rejected *before* the bytes are written (no oversize allocation).
//! - A partially-held file **longer** than the offer's declared `size` is corruption
//!   (a prefix that disagrees with what's being offered), so it rejects the whole
//!   transfer (`FileReject`) rather than clamping to `size` and accepting a too-long
//!   prefix as if it resumed cleanly.
//! - `FileAck.acked_through` is the **contiguous** committed prefix; resume restarts
//!   exactly there.
//! - On completion the reassembled SHA-256 is compared to the expected digest (when the
//!   caller supplied one). A mismatch — or any per-file finalize failure — does **not**
//!   commit the file as ok: it aborts the transfer locally *and* emits a
//!   `FileDone{ok:false}` for the peer, so the sender learns via the wire (not just a
//!   torn-down connection) and marks its side aborted.

use std::path::{Path, PathBuf};

use mouser_protocol::{
    FileAccept, FileAck, FileChunk, FileDone, FileOffer, FileReject, ResumePoint,
};

use crate::path::resolve_in_quarantine;
use crate::sink::FileSink;
use crate::{FileError, Hash, MAX_CHUNK_SIZE};

/// Per-file progress, exposed for tests/observability.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileState {
    /// Declared total size from the offer.
    pub size: u64,
    /// Contiguous bytes committed so far (== `FileAck.acked_through`).
    pub acked: u64,
    /// Whether this file has been finalized (size reached + hash, if any, verified).
    pub complete: bool,
}

/// One message the receiver wants sent back over the transfer's bulk stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outbound {
    Accept(FileAccept),
    Reject(FileReject),
    Ack(FileAck),
    Done(FileDone),
}

/// Static configuration for a receiver: where files land, the (optional) expected
/// per-file digests for the integrity gate, and the admission bounds that cap how much
/// an untrusted offer can ask the receiver to write (audit R2 — unbounded-disk DoS).
pub struct ReceiverConfig {
    /// Directory every received file is confined to (§7.8 quarantine).
    pub quarantine: PathBuf,
    /// Optional expected SHA-256 per file index. `None` ⇒ size-only completion check.
    /// An in-band `FileEntry.sha256` from the offer fills any `None` slot (C2-4); an
    /// out-of-band value supplied here takes precedence and must match if both are set.
    pub expected_hashes: Vec<Option<Hash>>,
    /// Max bytes any single offered file may declare. `None` ⇒ unbounded. Defaults to
    /// [`crate::DEFAULT_MAX_FILE_SIZE`] (set `None` via [`ReceiverConfig::with_limits`] to
    /// opt out).
    pub max_file_size: Option<u64>,
    /// Max number of files a single offer may list. `None` ⇒ unbounded. Defaults to
    /// [`crate::DEFAULT_MAX_FILES`].
    pub max_files: Option<usize>,
    /// Max total bytes (sum of all files' `size`) a single offer may declare. `None` ⇒
    /// unbounded. Defaults to [`crate::DEFAULT_MAX_TOTAL_BYTES`].
    pub max_total_bytes: Option<u64>,
}

impl ReceiverConfig {
    /// Quarantine config with **sane finite admission bounds by default** (audit R2 — an
    /// untrusted offer must not be able to ask for unbounded disk on the default path):
    /// [`crate::DEFAULT_MAX_FILE_SIZE`], [`crate::DEFAULT_MAX_FILES`], and
    /// [`crate::DEFAULT_MAX_TOTAL_BYTES`]. No expected hashes (completion gates on size,
    /// plus any in-band offer digest). Tighten or loosen with
    /// [`ReceiverConfig::with_limits`] (pass `None` for a dimension to make it unbounded).
    #[must_use]
    pub fn new(quarantine: PathBuf) -> Self {
        Self {
            quarantine,
            expected_hashes: Vec::new(),
            max_file_size: Some(crate::DEFAULT_MAX_FILE_SIZE),
            max_files: Some(crate::DEFAULT_MAX_FILES),
            max_total_bytes: Some(crate::DEFAULT_MAX_TOTAL_BYTES),
        }
    }

    /// Set the expected SHA-256 digests (index-aligned with the offer's `files`).
    #[must_use]
    pub fn with_expected_hashes(mut self, hashes: Vec<Option<Hash>>) -> Self {
        self.expected_hashes = hashes;
        self
    }

    /// Set the admission bounds (any `None` leaves that dimension unbounded). Checked in
    /// [`Receiver::accept_offer`] *before* any sink is opened; an offer that exceeds a
    /// bound is rejected with a `FileReject` and opens nothing.
    #[must_use]
    pub fn with_limits(
        mut self,
        max_file_size: Option<u64>,
        max_files: Option<usize>,
        max_total_bytes: Option<u64>,
    ) -> Self {
        self.max_file_size = max_file_size;
        self.max_files = max_files;
        self.max_total_bytes = max_total_bytes;
        self
    }
}

struct Slot<S> {
    size: u64,
    acked: u64,
    complete: bool,
    sink: S,
    expected: Option<Hash>,
}

/// The receiver half of a single `transfer_id`. Generic over the [`FileSink`] type so
/// tests use an in-memory sink and `mouser-engine` a disk-backed one.
pub struct Receiver<S: FileSink> {
    transfer_id: u64,
    quarantine: PathBuf,
    slots: Vec<Slot<S>>,
    done_emitted: bool,
    /// Set once a `FileDone{ok:false}` has been emitted (integrity/finalize failure);
    /// the transfer is terminally aborted and accepts no further chunks as progress.
    aborted: bool,
}

impl<S: FileSink> Receiver<S> {
    /// Handle a `FileOffer`: sanitize each name, build a sink per file via `make_sink`
    /// (passed the file index and the resolved safe path), and produce the messages to
    /// send back. The first is normally a `FileAccept` (with resume points for any
    /// partially-held files); on a path-safety violation, an over-long partial file, or
    /// a sink error it is instead a single `FileReject` that aborts the transfer. When a
    /// resume offer is already complete (the held prefix satisfies every file) a terminal
    /// `FileDone` follows the `FileAccept` in the returned vector.
    ///
    /// `make_sink` is where disk-backed receivers open the quarantine file (see
    /// [`crate::fs_sink::FsSink`], which `lstat`s and refuses a pre-existing symlink). It
    /// is only invoked after *every* file passes validation, so a rejected offer never
    /// leaves a half-opened sink behind.
    pub fn accept_offer<F>(
        offer: &FileOffer,
        config: ReceiverConfig,
        mut make_sink: F,
    ) -> Result<(Self, Vec<Outbound>), FileError>
    where
        F: FnMut(usize, &Path) -> Result<S, crate::sink::SinkError>,
    {
        if offer.files.is_empty() {
            return Err(FileError::Protocol("offer lists no files".into()));
        }
        let reject = |reason: String| {
            Ok((
                Self::aborted(offer.transfer_id, config.quarantine.clone()),
                vec![Outbound::Reject(FileReject {
                    transfer_id: offer.transfer_id,
                    reason,
                })],
            ))
        };

        // VALIDATE EVERYTHING before opening a single sink: admission bounds, then each
        // file's name safety + integrity digest. A reject here opens (and leaves behind)
        // nothing on disk (audit R2 — DoS bound + no orphan sinks on a rejected offer).
        if let Some(reason) = check_limits(offer, &config) {
            return reject(reason);
        }
        let mut plans = Vec::with_capacity(offer.files.len());
        for (i, entry) in offer.files.iter().enumerate() {
            match validate_entry(i, entry, &config) {
                Ok(plan) => plans.push(plan),
                Err(reason) => return reject(reason),
            }
        }

        // Only now open the sinks (one per validated file) and read resume offsets.
        let mut slots = Vec::with_capacity(plans.len());
        let mut resume = Vec::new();
        for (i, (safe_path, expected)) in plans.into_iter().enumerate() {
            let entry = offer.files.get(i).ok_or(FileError::UnknownFileIndex(
                u32::try_from(i).unwrap_or(u32::MAX),
            ))?;
            let sink = make_sink(i, &safe_path)?;
            // A partial file LONGER than the declared `size` is corruption/mismatch (the
            // prefix on disk disagrees with what's being offered). Reject the transfer
            // rather than clamp to `size` and accept a too-long prefix as a clean resume.
            let existing = sink.existing_len();
            if existing > entry.size {
                return reject(format!(
                    "file {i}: existing {existing} bytes exceed offered size {}",
                    entry.size
                ));
            }
            if existing > 0 {
                let file_index = u32::try_from(i)
                    .map_err(|_| FileError::Protocol("file_index overflow".into()))?;
                resume.push(ResumePoint {
                    file_index,
                    offset: existing,
                });
            }
            slots.push(Slot {
                size: entry.size,
                acked: existing,
                complete: false,
                sink,
                expected,
            });
        }

        let mut recv = Self {
            transfer_id: offer.transfer_id,
            quarantine: config.quarantine,
            slots,
            done_emitted: false,
            aborted: false,
        };
        let mut out = vec![Outbound::Accept(FileAccept {
            transfer_id: offer.transfer_id,
            resume,
        })];
        // A resume offer for already-complete files may finish immediately — surface the
        // terminal `FileDone` (ok=true on success, ok=false on an integrity mismatch).
        if let Some(done) = recv.finalize_completed_files()? {
            out.push(Outbound::Done(done));
        }
        Ok((recv, out))
    }

    fn aborted(transfer_id: u64, quarantine: PathBuf) -> Self {
        Self {
            transfer_id,
            quarantine,
            slots: Vec::new(),
            done_emitted: true,
            aborted: true,
        }
    }

    /// Handle a `FileChunk`: validate, write, and return the acks/done to send back.
    pub fn on_chunk(&mut self, chunk: &FileChunk) -> Result<Vec<Outbound>, FileError> {
        if chunk.transfer_id != self.transfer_id {
            return Err(FileError::UnknownTransfer(chunk.transfer_id));
        }
        if self.aborted {
            // Transfer already terminated (e.g. integrity failure): a `FileDone{ok:false}`
            // was sent; ignore any further chunks rather than resurrecting the transfer.
            return Ok(Vec::new());
        }
        if chunk.data.len() > MAX_CHUNK_SIZE {
            return Err(FileError::ChunkTooLarge(chunk.data.len()));
        }
        let idx = chunk.file_index as usize;
        let slot = self
            .slots
            .get_mut(idx)
            .ok_or(FileError::UnknownFileIndex(chunk.file_index))?;

        if slot.complete {
            // Already finished (duplicate/retransmit after resume) — re-ack idempotently.
            return Ok(vec![Outbound::Ack(FileAck {
                transfer_id: self.transfer_id,
                file_index: chunk.file_index,
                acked_through: slot.acked,
            })]);
        }

        let end = chunk.offset.checked_add(chunk.data.len() as u64).ok_or(
            FileError::OffsetOutOfRange {
                file_index: chunk.file_index,
                offset: chunk.offset,
            },
        )?;
        if end > slot.size {
            return Err(FileError::OffsetOutOfRange {
                file_index: chunk.file_index,
                offset: chunk.offset,
            });
        }

        // Reliable+ordered per-transfer stream ⇒ chunks arrive in order. A chunk wholly
        // at/below the ack point is a benign retransmit; a forward gap (`offset > acked`)
        // is a sender that got ahead of us. Neither is fatal: re-ack the contiguous prefix
        // we actually hold so the sender rewinds and retransmits from there (audit R2 —
        // a forward gap must NOT tear the connection down).
        if chunk.offset != slot.acked {
            return Ok(vec![Outbound::Ack(FileAck {
                transfer_id: self.transfer_id,
                file_index: chunk.file_index,
                acked_through: slot.acked,
            })]);
        }

        // A sink write failure is not recoverable for this file: abort the transfer and
        // tell the peer on the wire (`FileDone{ok:false}`) rather than swallowing it or
        // tearing the connection down (audit R2 — same terminal path as a hash mismatch).
        let wrote = slot.sink.write_at(slot.acked, &chunk.data);
        if wrote.is_ok() {
            slot.acked = end;
        }
        // The `slot` borrow ends here, freeing `self` for `abort`/`finalize` calls.
        if wrote.is_err() {
            return Ok(vec![Outbound::Done(self.abort())]);
        }

        let mut out = vec![Outbound::Ack(FileAck {
            transfer_id: self.transfer_id,
            file_index: chunk.file_index,
            acked_through: end,
        })];
        if let Some(done) = self.finalize_completed_files()? {
            out.push(Outbound::Done(done));
        }
        Ok(out)
    }

    /// Finalize any file that has reached its `size`, verifying the integrity hash when
    /// an expected digest was supplied.
    ///
    /// On success it returns a `FileDone{ok:true}` once *all* files complete. A SHA-256
    /// mismatch — or a failure to finalize the sink — does **not** mark the file complete:
    /// it aborts the transfer (so no corrupt file is committed as ok and no further chunk
    /// counts as progress) and returns `FileDone{ok:false}` so the caller can send it to
    /// the peer. The local [`FileError`] is *not* propagated for these cases; the abort is
    /// reported on the wire instead of via a torn-down connection.
    fn finalize_completed_files(&mut self) -> Result<Option<FileDone>, FileError> {
        for slot in &mut self.slots {
            if slot.complete || slot.acked < slot.size {
                continue;
            }
            // A finish() error or a digest mismatch both mean "this file is not ok".
            let ok = match slot.sink.finish() {
                Ok(digest) => slot.expected.is_none_or(|expected| digest == expected),
                Err(_) => false,
            };
            if !ok {
                return Ok(Some(self.abort()));
            }
            slot.complete = true;
        }
        if !self.done_emitted && self.slots.iter().all(|s| s.complete) {
            self.done_emitted = true;
            return Ok(Some(FileDone {
                transfer_id: self.transfer_id,
                ok: true,
            }));
        }
        Ok(None)
    }

    /// Mark the transfer terminally aborted and build the `FileDone{ok:false}` to send.
    fn abort(&mut self) -> FileDone {
        self.aborted = true;
        self.done_emitted = true;
        FileDone {
            transfer_id: self.transfer_id,
            ok: false,
        }
    }

    /// The quarantine directory this receiver writes into.
    #[must_use]
    pub fn quarantine(&self) -> &Path {
        &self.quarantine
    }

    /// Per-file progress snapshot (index-aligned with the offer).
    #[must_use]
    pub fn states(&self) -> Vec<FileState> {
        self.slots
            .iter()
            .map(|s| FileState {
                size: s.size,
                acked: s.acked,
                complete: s.complete,
            })
            .collect()
    }

    /// Whether every file has completed (and `FileDone{ok:true}` was produced).
    #[must_use]
    pub fn is_complete(&self) -> bool {
        !self.aborted && !self.slots.is_empty() && self.slots.iter().all(|s| s.complete)
    }

    /// Whether the transfer was terminally aborted with a `FileDone{ok:false}` (an
    /// integrity/finalize failure, or an over-long/unsafe offer that never opened sinks).
    #[must_use]
    pub fn is_aborted(&self) -> bool {
        self.aborted
    }

    /// Whether this receiver has reached a terminal state — either every file completed
    /// (`is_complete`) or the transfer aborted (`is_aborted`). A driver loops until this
    /// is true so an aborted transfer doesn't spin forever (audit R2).
    #[must_use]
    pub fn is_terminal(&self) -> bool {
        self.is_complete() || self.is_aborted()
    }
}

/// Validate one offered file *without touching disk*: resolve its name inside the
/// quarantine dir and settle its expected integrity digest. Returns the safe path and the
/// digest to verify on completion, or `Err(reason)` (turned into a `FileReject`).
///
/// Digest precedence (C2-4): an out-of-band hash from the config wins; otherwise the
/// offer's in-band `FileEntry.sha256` is adopted so completion is verified end-to-end. A
/// malformed in-band digest (not 32 bytes) or one that disagrees with an out-of-band hash
/// is rejected.
fn validate_entry(
    i: usize,
    entry: &mouser_protocol::FileEntry,
    config: &ReceiverConfig,
) -> Result<(PathBuf, Option<Hash>), String> {
    let safe_path = resolve_in_quarantine(&config.quarantine, &entry.name)
        .map_err(|e| format!("unsafe file name: {e}"))?;
    let out_of_band = config.expected_hashes.get(i).copied().flatten();
    let in_band = match entry.sha256.as_deref().map(Hash::try_from) {
        None => None,
        Some(Ok(h)) => Some(h),
        Some(Err(_)) => return Err(format!("file {i}: sha256 must be 32 bytes")),
    };
    if let (Some(a), Some(b)) = (out_of_band, in_band) {
        if a != b {
            return Err(format!("file {i}: offered sha256 disagrees with expected"));
        }
    }
    Ok((safe_path, out_of_band.or(in_band)))
}

/// Check the offer against the config's admission bounds. Returns `Some(reason)` for the
/// first violated bound (the receiver turns it into a `FileReject`), or `None` if the
/// offer is admissible. Pure; touches no disk and opens no sink.
fn check_limits(offer: &FileOffer, config: &ReceiverConfig) -> Option<String> {
    if let Some(max) = config.max_files {
        if offer.files.len() > max {
            return Some(format!(
                "offer lists {} files, exceeds max {max}",
                offer.files.len()
            ));
        }
    }
    let mut total: u64 = 0;
    for (i, entry) in offer.files.iter().enumerate() {
        if let Some(max) = config.max_file_size {
            if entry.size > max {
                return Some(format!(
                    "file {i}: size {} exceeds max file size {max}",
                    entry.size
                ));
            }
        }
        total = total.saturating_add(entry.size);
    }
    if let Some(max) = config.max_total_bytes {
        if total > max {
            return Some(format!("total size {total} exceeds max {max}"));
        }
    }
    None
}
