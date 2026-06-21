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
//! - `FileAck.acked_through` is the **contiguous** committed prefix; resume restarts
//!   exactly there.
//! - On completion the reassembled SHA-256 is compared to the expected digest (when the
//!   caller supplied one) — a mismatch fails the transfer with `FileDone{ok:false}`.

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

/// Static configuration for a receiver: where files land and the (optional) expected
/// per-file digests for the integrity gate.
pub struct ReceiverConfig {
    /// Directory every received file is confined to (§7.8 quarantine).
    pub quarantine: PathBuf,
    /// Optional expected SHA-256 per file index. `None` ⇒ size-only completion check.
    pub expected_hashes: Vec<Option<Hash>>,
}

impl ReceiverConfig {
    /// Quarantine-only config (no expected hashes — completion gates on size alone).
    #[must_use]
    pub fn new(quarantine: PathBuf) -> Self {
        Self {
            quarantine,
            expected_hashes: Vec::new(),
        }
    }

    /// Set the expected SHA-256 digests (index-aligned with the offer's `files`).
    #[must_use]
    pub fn with_expected_hashes(mut self, hashes: Vec<Option<Hash>>) -> Self {
        self.expected_hashes = hashes;
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
}

impl<S: FileSink> Receiver<S> {
    /// Handle a `FileOffer`: sanitize each name, build a sink per file via `make_sink`
    /// (passed the file index and the resolved safe path), and produce either a
    /// `FileAccept` (with resume points for any partially-held files) or, on a
    /// path-safety violation / sink error, a `FileReject` that aborts the transfer.
    ///
    /// `make_sink` is where disk-backed receivers open the quarantine file (with
    /// `create_new`, never following a symlink).
    pub fn accept_offer<F>(
        offer: &FileOffer,
        config: ReceiverConfig,
        mut make_sink: F,
    ) -> Result<(Self, Outbound), FileError>
    where
        F: FnMut(usize, &Path) -> Result<S, crate::sink::SinkError>,
    {
        if offer.files.is_empty() {
            return Err(FileError::Protocol("offer lists no files".into()));
        }
        let mut slots = Vec::with_capacity(offer.files.len());
        let mut resume = Vec::new();

        for (i, entry) in offer.files.iter().enumerate() {
            // Path safety FIRST — before any sink/disk touch. A bad name rejects the
            // whole transfer rather than silently renaming (defence over convenience).
            let safe_path = match resolve_in_quarantine(&config.quarantine, &entry.name) {
                Ok(p) => p,
                Err(e) => {
                    return Ok((
                        Self::aborted(offer.transfer_id, config.quarantine),
                        Outbound::Reject(FileReject {
                            transfer_id: offer.transfer_id,
                            reason: format!("unsafe file name: {e}"),
                        }),
                    ));
                }
            };
            let sink = make_sink(i, &safe_path)?;
            let have = sink.existing_len().min(entry.size);
            if have > 0 {
                let file_index = u32::try_from(i)
                    .map_err(|_| FileError::Protocol("file_index overflow".into()))?;
                resume.push(ResumePoint {
                    file_index,
                    offset: have,
                });
            }
            let expected = config.expected_hashes.get(i).copied().flatten();
            slots.push(Slot {
                size: entry.size,
                acked: have,
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
        };
        // A resume offer for already-complete files may finish immediately.
        recv.finalize_completed_files()?;
        let accept = Outbound::Accept(FileAccept {
            transfer_id: offer.transfer_id,
            resume,
        });
        Ok((recv, accept))
    }

    fn aborted(transfer_id: u64, quarantine: PathBuf) -> Self {
        Self {
            transfer_id,
            quarantine,
            slots: Vec::new(),
            done_emitted: true,
        }
    }

    /// Handle a `FileChunk`: validate, write, and return the acks/done to send back.
    pub fn on_chunk(&mut self, chunk: &FileChunk) -> Result<Vec<Outbound>, FileError> {
        if chunk.transfer_id != self.transfer_id {
            return Err(FileError::UnknownTransfer(chunk.transfer_id));
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

        let end = chunk
            .offset
            .checked_add(chunk.data.len() as u64)
            .ok_or(FileError::OffsetOutOfRange {
                file_index: chunk.file_index,
                offset: chunk.offset,
            })?;
        if end > slot.size {
            return Err(FileError::OffsetOutOfRange {
                file_index: chunk.file_index,
                offset: chunk.offset,
            });
        }

        // Reliable+ordered per-transfer stream ⇒ chunks arrive in order. A chunk wholly
        // at/below the ack point is a benign retransmit (drop, re-ack); a forward gap is
        // a protocol violation on this transport.
        if chunk.offset < slot.acked {
            return Ok(vec![Outbound::Ack(FileAck {
                transfer_id: self.transfer_id,
                file_index: chunk.file_index,
                acked_through: slot.acked,
            })]);
        }
        if chunk.offset > slot.acked {
            return Err(FileError::OffsetOutOfRange {
                file_index: chunk.file_index,
                offset: chunk.offset,
            });
        }

        slot.sink.write_at(slot.acked, &chunk.data)?;
        slot.acked = end;

        let mut out = vec![Outbound::Ack(FileAck {
            transfer_id: self.transfer_id,
            file_index: chunk.file_index,
            acked_through: slot.acked,
        })];
        if let Some(done) = self.finalize_completed_files()? {
            out.push(Outbound::Done(done));
        }
        Ok(out)
    }

    /// Finalize any file that has reached its `size`, verifying the integrity hash when
    /// an expected digest was supplied. Returns a `FileDone` once *all* files complete.
    fn finalize_completed_files(&mut self) -> Result<Option<FileDone>, FileError> {
        for (i, slot) in self.slots.iter_mut().enumerate() {
            if slot.complete || slot.acked < slot.size {
                continue;
            }
            let digest = slot.sink.finish()?;
            if let Some(expected) = slot.expected {
                if digest != expected {
                    self.done_emitted = true;
                    return Err(FileError::HashMismatch {
                        file_index: u32::try_from(i).unwrap_or(u32::MAX),
                    });
                }
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
        !self.slots.is_empty() && self.slots.iter().all(|s| s.complete)
    }
}
