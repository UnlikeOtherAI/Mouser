//! The narrow I/O seam that keeps the clipboard engine pure, plus an in-memory fake
//! for tests. The state machine touches the OS clipboard **only** through these types,
//! so all logic is unit-testable with no platform calls.
//!
//! Two directions:
//! - **Outbound** ([`ClipContentSource`]): when the engine must answer an inbound
//!   `ClipboardPull`, it asks the source for the *canonical* bytes of one `(format,
//!   hash)` it previously offered. Returning `None` means "I no longer hold that"
//!   (the clipboard moved on) and the pull is dropped.
//! - **Inbound** (the engine returns [`AppliedClip`] to the caller): once a pulled
//!   payload reassembles and its hash verifies, the engine hands the completed
//!   `(format, bytes)` back so the caller can write it to the OS clipboard. The write
//!   itself is the caller's (adapter's) job — kept out of the pure engine.
//!
//! A locally-available representation is described by [`LocalRepr`]: the raw bytes the
//! engine canonicalizes + hashes to build a `ClipboardOffer`. The caller supplies one
//! `LocalRepr` per format the OS clipboard currently holds.

use mouser_protocol::ClipFormat;

use crate::Hash;

/// One representation the local clipboard currently holds, as raw (un-canonicalized)
/// bytes. The engine canonicalizes per §7.7 and hashes to build the offer entry, so
/// the caller hands over exactly what the OS reported for `format`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocalRepr {
    /// The representation's format.
    pub format: ClipFormat,
    /// Raw bytes as read from the OS clipboard for `format` (pre-canonicalization).
    pub bytes: Vec<u8>,
}

impl LocalRepr {
    /// Convenience constructor.
    #[must_use]
    pub fn new(format: ClipFormat, bytes: Vec<u8>) -> Self {
        Self { format, bytes }
    }
}

/// A completed inbound clipboard payload the engine has reassembled and verified
/// (hash matched the offer). The caller writes `bytes` to the OS clipboard for
/// `format`; the bytes are the **canonical** form (what was hashed), ready to apply.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppliedClip {
    /// Format to write to the OS clipboard.
    pub format: ClipFormat,
    /// Canonical, hash-verified bytes to write.
    pub bytes: Vec<u8>,
}

/// The outbound content seam: given a `(format, hash)` the local device previously
/// offered, return the **canonical** bytes to stream back in `ClipboardData`, or
/// `None` if the content is no longer available (clipboard moved on / unknown hash).
///
/// Implementations are pure data lookups — no blocking I/O on the engine's hot path is
/// assumed (a real adapter snapshots the canonical bytes when it builds the offer).
pub trait ClipContentSource {
    /// Canonical bytes for one previously-offered representation, or `None`.
    fn canonical_bytes(&self, format: ClipFormat, hash: &Hash) -> Option<Vec<u8>>;
}

/// An in-memory [`ClipContentSource`] for tests: maps `(format, hash)` → canonical
/// bytes. Mirrors how a real adapter snapshots each offered representation.
#[derive(Clone, Debug, Default)]
pub struct MemContentSource {
    entries: Vec<(ClipFormat, Hash, Vec<u8>)>,
}

impl MemContentSource {
    /// An empty source (every lookup misses).
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert one representation's canonical `bytes` under `(format, hash)`. The hash
    /// is the caller's responsibility to compute (normally via
    /// [`crate::content_hash`]); tests usually build this from the same `LocalRepr`s
    /// they offer.
    pub fn insert(&mut self, format: ClipFormat, hash: Hash, bytes: Vec<u8>) {
        self.entries.push((format, hash, bytes));
    }
}

impl ClipContentSource for MemContentSource {
    fn canonical_bytes(&self, format: ClipFormat, hash: &Hash) -> Option<Vec<u8>> {
        self.entries
            .iter()
            .find(|(f, h, _)| *f == format && h == hash)
            .map(|(_, _, b)| b.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::content_hash;

    #[test]
    fn mem_source_round_trips_by_format_and_hash() {
        let mut src = MemContentSource::new();
        let bytes = b"hello".to_vec();
        let h = content_hash(ClipFormat::Utf8Text, &bytes);
        src.insert(ClipFormat::Utf8Text, h, bytes.clone());

        assert_eq!(src.canonical_bytes(ClipFormat::Utf8Text, &h), Some(bytes));
        // wrong format misses.
        assert_eq!(src.canonical_bytes(ClipFormat::Html, &h), None);
        // wrong hash misses.
        assert_eq!(src.canonical_bytes(ClipFormat::Utf8Text, &[0u8; 32]), None);
    }
}
