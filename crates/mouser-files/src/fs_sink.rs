//! [`FsSink`] — the production, disk-backed [`FileSink`] (audit R2 **C2-5**).
//!
//! The two §7.8 properties that the old test sink could not satisfy together —
//! *resume* (reopen a partial quarantine file and continue) and *symlink-safety*
//! (never write through a pre-existing symlink) — are reconciled here:
//!
//! 1. **lstat before open.** [`std::fs::symlink_metadata`] (which does *not* follow a
//!    final symlink) checks the target: if it already exists and is a symlink, the open
//!    is refused ([`SinkError`]). A regular file is allowed (that is the resume case); a
//!    non-existent path is allowed (fresh download). This replaces `create_new(true)`,
//!    which made resume impossible.
//! 2. **Positioned writes.** Bytes are committed with
//!    [`std::os::unix::fs::FileExt::write_all_at`] at the chunk's `offset`, so a
//!    retransmit/resume writes to the right place instead of appending. The engine only
//!    ever calls with `offset == existing_len()`; that invariant is asserted here, so a
//!    gap (`offset > len`) or a rewrite (`offset < len`) is a hard [`SinkError`] rather
//!    than silent corruption.
//! 3. **Streaming hash, never a re-read.** A [`Sha256`] is rolled forward in `write_at`
//!    and returned from `finish()`. The path is **never** re-read to compute the digest,
//!    so there is no TOCTOU window where the file could be swapped between the last write
//!    and the integrity check.
//!
//! A resumed sink (an existing partial file) seeds the rolling hash from the bytes
//! already on disk in [`FsSink::open`], so the digest at `finish()` covers the whole file
//! regardless of how many legs the transfer took.
//!
//! Unix-only: positioned writes use the Unix `FileExt`. The path itself is validated to be
//! inside the quarantine dir purely by [`crate::path`] before this sink is ever opened.

#![cfg(unix)]

use std::fs::{File, OpenOptions};
use std::io::Read;
use std::os::unix::fs::FileExt;
use std::path::Path;

use sha2::{Digest, Sha256};

use crate::sink::{FileSink, SinkError};
use crate::Hash;

/// A disk-backed [`FileSink`] that writes received bytes into a single quarantine file,
/// supporting resume and refusing to follow a pre-existing symlink. Construct one per
/// file via [`FsSink::open`] (typically from the `make_sink` closure passed to
/// [`crate::Receiver::accept_offer`]).
pub struct FsSink {
    file: File,
    /// Bytes durably written so far (the resume point). Advanced by each `write_at`.
    len: u64,
    /// Rolling SHA-256 over every byte written (seeded from any pre-existing prefix).
    hasher: Sha256,
}

impl FsSink {
    /// Open `path` for resumable, symlink-safe writing.
    ///
    /// Rejects (as [`SinkError`]) a path whose final component is an existing symlink.
    /// An existing regular file is reopened for resume: its current length becomes the
    /// resume point and its bytes seed the rolling hash. A non-existent path is created.
    ///
    /// `path` MUST already have passed [`crate::path::resolve_in_quarantine`]; this is the
    /// on-disk half of §7.8's "no symlink follow", not a substitute for name sanitization.
    pub fn open(path: &Path) -> Result<Self, SinkError> {
        // lstat WITHOUT following a final symlink. Existing symlink ⇒ refuse; missing ⇒
        // fresh file; existing regular file ⇒ resume.
        let (resume_len, seed) = match std::fs::symlink_metadata(path) {
            Ok(meta) => {
                if meta.file_type().is_symlink() {
                    return Err(SinkError(format!(
                        "refusing to write through symlink: {}",
                        path.display()
                    )));
                }
                (meta.len(), true)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (0, false),
            Err(e) => return Err(SinkError(format!("lstat {}: {e}", path.display()))),
        };

        // write + create, but NOT create_new (resume needs to reopen) and NOT truncate
        // (resume must keep the prefix). The lstat above already rejected a symlink target.
        let file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)
            .map_err(|e| SinkError(format!("open {}: {e}", path.display())))?;

        let mut hasher = Sha256::new();
        if seed && resume_len > 0 {
            // Seed the rolling hash from the bytes already on disk so the final digest
            // covers the whole file. Read sequentially with a bounded buffer.
            let mut reader = File::open(path)
                .map_err(|e| SinkError(format!("reopen-for-hash {}: {e}", path.display())))?;
            let mut buf = [0u8; 64 * 1024];
            let mut hashed: u64 = 0;
            while hashed < resume_len {
                let n = reader
                    .read(&mut buf)
                    .map_err(|e| SinkError(format!("seed-hash read: {e}")))?;
                if n == 0 {
                    break;
                }
                let take = usize::try_from(resume_len - hashed)
                    .map(|rem| rem.min(n))
                    .unwrap_or(n);
                let slice = buf
                    .get(..take)
                    .ok_or_else(|| SinkError("seed-hash slice out of range".into()))?;
                hasher.update(slice);
                hashed += take as u64;
            }
            if hashed != resume_len {
                return Err(SinkError(format!(
                    "seed-hash short read: hashed {hashed} of {resume_len}"
                )));
            }
        }

        Ok(Self {
            file,
            len: resume_len,
            hasher,
        })
    }
}

impl FileSink for FsSink {
    fn existing_len(&self) -> u64 {
        self.len
    }

    fn write_at(&mut self, offset: u64, data: &[u8]) -> Result<(), SinkError> {
        // The engine commits strictly contiguously; anything else is a bug or a hostile
        // peer that slipped past validation — never write to the wrong place.
        if offset != self.len {
            return Err(SinkError(format!(
                "non-contiguous write at {offset}, have {} (gap or rewrite)",
                self.len
            )));
        }
        self.file
            .write_all_at(data, offset)
            .map_err(|e| SinkError(format!("write_all_at {offset}: {e}")))?;
        self.hasher.update(data);
        self.len = self
            .len
            .checked_add(data.len() as u64)
            .ok_or_else(|| SinkError("length overflow".into()))?;
        Ok(())
    }

    fn finish(&mut self) -> Result<Hash, SinkError> {
        // Flush to disk, then return the STREAMING digest — the path is never re-read, so
        // there is no TOCTOU window between the last write and the integrity check.
        self.file
            .sync_all()
            .map_err(|e| SinkError(format!("sync_all: {e}")))?;
        Ok(self.hasher.clone().finalize().into())
    }
}
