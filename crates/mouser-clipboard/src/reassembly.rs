//! Inbound `ClipboardData` reassembly with progress + hash verification (§7.7).
//!
//! A pulled representation arrives as one or more ordered `ClipboardData` chunks for a
//! single `hash`. This accumulator commits the **contiguous** byte prefix (the bulk
//! plane is reliable+ordered per `hash` stream, §11), exposes [`Progress`] so the UI
//! can show a "Pasting from <device>…" indicator, and on the `last` chunk verifies the
//! reassembled SHA-256 against the offered `hash` — dropping the payload on mismatch
//! rather than applying corrupt bytes.

use mouser_protocol::ClipFormat;

use crate::canonical::sha256;
use crate::{ClipboardError, Hash};

/// Progress of an in-flight pull (§7.7 "wait" indicator): bytes committed so far out
/// of the offer's declared total.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Progress {
    /// Contiguous bytes reassembled so far (`Σ data.len()` of committed chunks).
    pub received_bytes: u64,
    /// Total expected size from the offer (`ClipboardEntry.size`).
    pub size: u64,
}

impl Progress {
    /// Fraction received in `[0.0, 1.0]`. A zero-size payload is reported complete
    /// (`1.0`) so an empty clipboard doesn't show a stuck indicator.
    #[must_use]
    pub fn fraction(self) -> f64 {
        if self.size == 0 {
            return 1.0;
        }
        // received_bytes is always ≤ size (the accumulator caps it), so this is ≤ 1.0.
        self.received_bytes as f64 / self.size as f64
    }

    /// Whether every expected byte has been received.
    #[must_use]
    pub fn is_complete(self) -> bool {
        self.received_bytes >= self.size
    }
}

/// Accumulates the ordered `ClipboardData` chunks for one pulled `hash`.
pub struct Reassembly {
    format: ClipFormat,
    hash: Hash,
    size: u64,
    buf: Vec<u8>,
    done: bool,
}

impl Reassembly {
    /// Start reassembling a representation of `format`, expecting `size` bytes that
    /// canonicalize to `hash`.
    #[must_use]
    pub fn new(format: ClipFormat, hash: Hash, size: u64) -> Self {
        Self {
            format,
            hash,
            size,
            buf: Vec::new(),
            done: false,
        }
    }

    /// The format being reassembled.
    #[must_use]
    pub fn format(&self) -> ClipFormat {
        self.format
    }

    /// The content hash this payload must verify against.
    #[must_use]
    pub fn hash(&self) -> &Hash {
        &self.hash
    }

    /// Current [`Progress`].
    #[must_use]
    pub fn progress(&self) -> Progress {
        Progress {
            received_bytes: self.buf.len() as u64,
            size: self.size,
        }
    }

    /// Commit one ordered chunk.
    ///
    /// Validates that `offset` is exactly the current contiguous end (the stream is
    /// ordered), that the chunk does not exceed [`crate::MAX_DATA_CHUNK`] (1 MiB), and
    /// that committing it does not run past the offered `size`. On the `last` chunk it
    /// verifies the reassembled SHA-256 against `hash` and, on success, returns the
    /// completed canonical bytes; a mismatch returns [`ClipboardError::HashMismatch`]
    /// so the caller drops the pending payload. Returns `Ok(None)` while more chunks
    /// are expected.
    pub fn push(
        &mut self,
        offset: u64,
        data: &[u8],
        last: bool,
    ) -> Result<Option<Vec<u8>>, ClipboardError> {
        if self.done {
            return Err(ClipboardError::Protocol("chunk after last".into()));
        }
        if data.len() > crate::MAX_DATA_CHUNK {
            return Err(ClipboardError::ChunkTooLarge(data.len()));
        }
        let committed = self.buf.len() as u64;
        if offset != committed {
            return Err(ClipboardError::OffsetOutOfRange {
                expected: committed,
                got: offset,
            });
        }
        let end =
            offset
                .checked_add(data.len() as u64)
                .ok_or(ClipboardError::OffsetOutOfRange {
                    expected: committed,
                    got: offset,
                })?;
        if end > self.size {
            return Err(ClipboardError::OffsetOutOfRange {
                expected: self.size,
                got: end,
            });
        }
        self.buf.extend_from_slice(data);

        if !last {
            return Ok(None);
        }
        self.done = true;
        // The final chunk must complete the declared size — a `last` short of `size`
        // is a truncated payload and must not be applied.
        if (self.buf.len() as u64) != self.size {
            return Err(ClipboardError::OffsetOutOfRange {
                expected: self.size,
                got: self.buf.len() as u64,
            });
        }
        // The reassembled bytes are already canonical (the sender streams canonical
        // bytes), so hash them directly and compare to the offered hash.
        if sha256(&self.buf) != self.hash {
            return Err(ClipboardError::HashMismatch);
        }
        Ok(Some(std::mem::take(&mut self.buf)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canonical::content_hash;

    #[test]
    fn single_chunk_verifies_and_completes() {
        let bytes = b"clipboard".to_vec();
        let h = content_hash(ClipFormat::Utf8Text, &bytes);
        let mut r = Reassembly::new(ClipFormat::Utf8Text, h, bytes.len() as u64);
        assert_eq!(r.progress().received_bytes, 0);
        // The completion signal is the returned `Some(bytes)`; the buffer is taken on
        // completion, so progress is not inspected afterwards.
        let out = r.push(0, &bytes, true).expect("ok");
        assert_eq!(out, Some(bytes));
    }

    #[test]
    fn multi_chunk_progress_then_complete() {
        let bytes = vec![7u8; 10];
        let h = content_hash(ClipFormat::Png, &bytes);
        let mut r = Reassembly::new(ClipFormat::Png, h, 10);
        let (head, tail) = bytes.split_at(4);
        assert_eq!(r.push(0, head, false).expect("c0"), None);
        assert_eq!(r.progress().received_bytes, 4);
        assert!((r.progress().fraction() - 0.4).abs() < 1e-9);
        assert_eq!(r.push(4, tail, true).expect("c1"), Some(bytes));
    }

    #[test]
    fn out_of_order_offset_rejected() {
        let h = content_hash(ClipFormat::Utf8Text, b"abcd");
        let mut r = Reassembly::new(ClipFormat::Utf8Text, h, 4);
        assert_eq!(
            r.push(2, b"cd", true),
            Err(ClipboardError::OffsetOutOfRange {
                expected: 0,
                got: 2
            })
        );
    }

    #[test]
    fn past_size_rejected() {
        let h = content_hash(ClipFormat::Utf8Text, b"ab");
        let mut r = Reassembly::new(ClipFormat::Utf8Text, h, 2);
        assert!(matches!(
            r.push(0, b"abc", true),
            Err(ClipboardError::OffsetOutOfRange { .. })
        ));
    }

    #[test]
    fn hash_mismatch_drops_payload() {
        // declared hash is for "right" but we feed "wrong" of the same length.
        let h = content_hash(ClipFormat::Utf8Text, b"right");
        let mut r = Reassembly::new(ClipFormat::Utf8Text, h, 5);
        assert_eq!(r.push(0, b"wrong", true), Err(ClipboardError::HashMismatch));
    }

    #[test]
    fn truncated_last_rejected() {
        let h = content_hash(ClipFormat::Utf8Text, b"abcd");
        let mut r = Reassembly::new(ClipFormat::Utf8Text, h, 4);
        // `last` arrives with only 2 of 4 bytes committed.
        assert!(matches!(
            r.push(0, b"ab", true),
            Err(ClipboardError::OffsetOutOfRange { .. })
        ));
    }

    #[test]
    fn oversize_chunk_rejected_before_commit() {
        let h = content_hash(ClipFormat::Png, &[0u8; 4]);
        let mut r = Reassembly::new(ClipFormat::Png, h, crate::MAX_DATA_CHUNK as u64 + 4);
        let huge = vec![0u8; crate::MAX_DATA_CHUNK + 1];
        assert_eq!(
            r.push(0, &huge, false),
            Err(ClipboardError::ChunkTooLarge(crate::MAX_DATA_CHUNK + 1))
        );
    }
}
