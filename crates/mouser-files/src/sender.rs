//! The **sender** state machine (§7.8). Builds the `FileOffer`, applies the
//! receiver's `FileAccept` resume points, then emits `FileChunk`s under a per-file
//! cumulative-ack window (≤ [`crate::MAX_IN_FLIGHT_PER_FILE`] = 8 MiB unacked, §0.3),
//! advancing as `FileAck`s arrive. Pure: bytes come from a [`FileSource`].
//!
//! Drive loop (the engine repeats until [`Sender::is_complete`]):
//! ```text
//! send offer; on FileAccept call on_accept; then loop:
//!   while let Some(chunk) = poll_chunk()? { send chunk }
//!   on each inbound FileAck call on_ack (re-opens window), then poll again
//! ```

use mouser_protocol::{FileAccept, FileAck, FileChunk, FileDone, FileEntry, FileOffer};

use crate::sink::FileSource;
use crate::{FileError, DEFAULT_CHUNK_SIZE, MAX_IN_FLIGHT_PER_FILE};

struct Track<Src> {
    name: String,
    src: Src,
    size: u64,
    /// Optional precomputed SHA-256 of the file, advertised in the offer's `FileEntry`
    /// so the receiver can verify integrity end-to-end (C2-4). `None` ⇒ no in-band hash.
    sha256: Option<crate::Hash>,
    /// Bytes already emitted in chunks (the high-water of what's been put on the wire).
    sent: u64,
    /// Bytes the receiver has cumulatively acked (`FileAck.acked_through`).
    acked: u64,
    /// Set once a resume point has been applied for this file, so a duplicate
    /// `file_index` in `FileAccept.resume` is rejected rather than re-applied.
    resumed: bool,
}

/// The sender half of a single `transfer_id`. Generic over the [`FileSource`] type.
pub struct Sender<Src: FileSource> {
    transfer_id: u64,
    tracks: Vec<Track<Src>>,
    chunk_size: usize,
    accepted: bool,
    aborted: bool,
}

impl<Src: FileSource> Sender<Src> {
    /// Build a sender for `files` (each `(name, source)`); the offered `size` is the
    /// source's length. No per-file digest is advertised — use [`Sender::new_with_hashes`]
    /// to carry SHA-256s in the offer (C2-4). Fails if the list is empty or has more than
    /// `u32::MAX` files.
    pub fn new(transfer_id: u64, files: Vec<(String, Src)>) -> Result<Self, FileError> {
        Self::new_with_hashes(
            transfer_id,
            files.into_iter().map(|(n, s)| (n, s, None)).collect(),
        )
    }

    /// Like [`Sender::new`] but each file carries an optional precomputed SHA-256 that is
    /// advertised in the offer (`FileEntry.sha256`) so the receiver can verify integrity
    /// end-to-end (C2-4).
    pub fn new_with_hashes(
        transfer_id: u64,
        files: Vec<(String, Src, Option<crate::Hash>)>,
    ) -> Result<Self, FileError> {
        if files.is_empty() {
            return Err(FileError::Protocol("nothing to send".into()));
        }
        u32::try_from(files.len()).map_err(|_| FileError::Protocol("too many files".into()))?;
        let tracks = files
            .into_iter()
            .map(|(name, src, sha256)| {
                let size = src.len();
                Track {
                    name,
                    src,
                    size,
                    sha256,
                    sent: 0,
                    acked: 0,
                    resumed: false,
                }
            })
            .collect();
        Ok(Self {
            transfer_id,
            tracks,
            chunk_size: DEFAULT_CHUNK_SIZE,
            accepted: false,
            aborted: false,
        })
    }

    /// Override the chunk size the sender emits (must be ≥ 1 and ≤ 1 MiB, §0.3).
    pub fn with_chunk_size(mut self, n: usize) -> Result<Self, FileError> {
        if n == 0 || n > crate::MAX_CHUNK_SIZE {
            return Err(FileError::Protocol("chunk size out of range".into()));
        }
        self.chunk_size = n;
        Ok(self)
    }

    /// The `FileOffer` to send first (one [`FileEntry`] per file). Each entry carries the
    /// file's SHA-256 when one was supplied (C2-4), encoded as a CBOR byte string.
    #[must_use]
    pub fn offer(&self) -> FileOffer {
        FileOffer {
            transfer_id: self.transfer_id,
            files: self
                .tracks
                .iter()
                .map(|t| FileEntry {
                    name: t.name.clone(),
                    size: t.size,
                    sha256: t.sha256.map(|h| h.to_vec()),
                })
                .collect(),
        }
    }

    /// Apply the receiver's `FileAccept`: fast-forward each named file's `sent`/`acked`
    /// to the resume offset so the window starts past bytes the receiver already holds.
    ///
    /// Single-shot and resume-trusting (audit R2): a second `FileAccept` is rejected
    /// (`Protocol`), as is a `resume` list with a duplicate `file_index` — the receiver
    /// declares each file's resume point exactly once, and the offset stays bounded by the
    /// file `size`.
    pub fn on_accept(&mut self, accept: &FileAccept) -> Result<(), FileError> {
        if accept.transfer_id != self.transfer_id {
            return Err(FileError::UnknownTransfer(accept.transfer_id));
        }
        if self.accepted {
            return Err(FileError::Protocol("duplicate FileAccept".into()));
        }
        // Validate the ENTIRE resume vec before mutating any track (audit R2 — atomic
        // resume): a duplicate `file_index`, an unknown index, or an out-of-range offset
        // anywhere in the list rejects the whole accept and leaves the sender untouched, so
        // a later valid accept still applies cleanly (no poisoned partial state).
        for (pos, r) in accept.resume.iter().enumerate() {
            let t = self
                .tracks
                .get(r.file_index as usize)
                .ok_or(FileError::UnknownFileIndex(r.file_index))?;
            if r.offset > t.size {
                return Err(FileError::OffsetOutOfRange {
                    file_index: r.file_index,
                    offset: r.offset,
                });
            }
            // A duplicate `file_index` is a malformed accept (each file's resume point is
            // declared exactly once). Detected against the already-seen prefix without
            // touching track state.
            if accept
                .resume
                .get(..pos)
                .is_some_and(|earlier| earlier.iter().any(|e| e.file_index == r.file_index))
            {
                return Err(FileError::Protocol(format!(
                    "duplicate resume for file_index {}",
                    r.file_index
                )));
            }
        }
        // All checks passed — now apply the resume points.
        for r in &accept.resume {
            let t = self
                .tracks
                .get_mut(r.file_index as usize)
                .ok_or(FileError::UnknownFileIndex(r.file_index))?;
            t.resumed = true;
            t.sent = r.offset;
            t.acked = r.offset;
        }
        self.accepted = true;
        Ok(())
    }

    /// Apply a `FileAck`, advancing the cumulative ack (and thus re-opening the window).
    ///
    /// An ack can only ever confirm bytes that were actually put on the wire, so it is
    /// rejected (audit C2-4 — "trust the ack") when:
    /// - the offer has not been accepted yet (no bytes can have been sent),
    /// - `acked_through` exceeds `sent` (the peer claims bytes we never transmitted —
    ///   which would otherwise let it mark a file complete before any chunk was sent), or
    /// - `acked_through` exceeds the file `size`.
    ///
    /// A stale (lower-or-equal) ack is a benign duplicate: it is accepted but never moves
    /// `acked` backwards (cumulative acks are monotonic).
    pub fn on_ack(&mut self, ack: &FileAck) -> Result<(), FileError> {
        if ack.transfer_id != self.transfer_id {
            return Err(FileError::UnknownTransfer(ack.transfer_id));
        }
        if !self.accepted {
            return Err(FileError::Protocol("FileAck before FileAccept".into()));
        }
        let t = self
            .tracks
            .get_mut(ack.file_index as usize)
            .ok_or(FileError::UnknownFileIndex(ack.file_index))?;
        if ack.acked_through > t.size || ack.acked_through > t.sent {
            // Either past the declared size or past what we actually sent — an impossible
            // ack the peer must not be able to use to "complete" a file early.
            return Err(FileError::OffsetOutOfRange {
                file_index: ack.file_index,
                offset: ack.acked_through,
            });
        }
        // Cumulative acks only ever move forward; a stale (lower) ack is ignored so `acked`
        // never regresses.
        if ack.acked_through > t.acked {
            t.acked = ack.acked_through;
        }
        Ok(())
    }

    /// Record the terminal `FileDone`; `ok = false` marks the transfer aborted.
    pub fn on_done(&mut self, done: &FileDone) -> Result<(), FileError> {
        if done.transfer_id != self.transfer_id {
            return Err(FileError::UnknownTransfer(done.transfer_id));
        }
        if !done.ok {
            self.aborted = true;
        }
        Ok(())
    }

    /// Produce the next `FileChunk` to send, or `None` if nothing is sendable right now
    /// (offer not yet accepted, every file fully sent, or every file's 8 MiB window is
    /// full pending acks). The engine calls this repeatedly between acks.
    pub fn poll_chunk(&mut self) -> Result<Option<FileChunk>, FileError> {
        if !self.accepted || self.aborted {
            return Ok(None);
        }
        for (i, t) in self.tracks.iter_mut().enumerate() {
            if t.sent >= t.size {
                continue;
            }
            let in_flight = t.sent.saturating_sub(t.acked);
            if in_flight >= MAX_IN_FLIGHT_PER_FILE {
                continue; // window full for this file; try the next
            }
            let window_left = MAX_IN_FLIGHT_PER_FILE - in_flight;
            let file_left = t.size - t.sent;
            let want = (self.chunk_size as u64).min(window_left).min(file_left);
            let want = usize::try_from(want)
                .map_err(|_| FileError::Protocol("chunk length overflow".into()))?;
            let mut buf = vec![0u8; want];
            let n = t.src.read_at(t.sent, &mut buf)?;
            if n == 0 {
                // Source claims fewer bytes than `size` advertised — treat as a fault.
                return Err(FileError::OffsetOutOfRange {
                    file_index: u32::try_from(i).unwrap_or(u32::MAX),
                    offset: t.sent,
                });
            }
            buf.truncate(n);
            let offset = t.sent;
            t.sent += n as u64;
            return Ok(Some(FileChunk {
                transfer_id: self.transfer_id,
                file_index: u32::try_from(i).unwrap_or(u32::MAX),
                offset,
                data: buf,
            }));
        }
        Ok(None)
    }

    /// Whether every file's bytes have been fully acked (transfer succeeded).
    #[must_use]
    pub fn is_complete(&self) -> bool {
        !self.aborted && self.tracks.iter().all(|t| t.acked >= t.size)
    }

    /// Whether a `FileDone{ok:false}` aborted the transfer.
    #[must_use]
    pub fn is_aborted(&self) -> bool {
        self.aborted
    }
}
