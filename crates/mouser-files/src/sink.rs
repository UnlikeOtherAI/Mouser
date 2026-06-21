//! The two narrow I/O traits that keep the transfer engine pure, plus in-memory
//! implementations for tests. The state machines call only these — file/socket I/O
//! lives behind them so `mouser-engine` can plug real disk-backed types in.
//!
//! - [`FileSource`] — the **sender** side: random-access read of the bytes to send.
//! - [`FileSink`] — the **receiver** side: append validated contiguous bytes and, on
//!   completion, report the SHA-256 of everything written (the integrity oracle).
//!
//! A real receiver sink opens its quarantine file with `create_new` so it never
//! follows a pre-existing symlink (the on-disk half of §7.8's "no symlink follow";
//! [`crate::path`] handles the name half purely).

/// An I/O failure from a [`FileSource`] or [`FileSink`]. Kept stringly-typed so any
/// backing store (memory, `std::fs`, a future async file) maps cleanly without leaking
/// its error type into the pure engine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SinkError(pub String);

impl core::fmt::Display for SinkError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for SinkError {}

/// Random-access source of the bytes the **sender** transmits.
pub trait FileSource {
    /// Total length of this file in bytes (must equal the `size` the sender offers).
    fn len(&self) -> u64;

    /// Whether the file is empty (clippy's companion to [`FileSource::len`]).
    fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Read up to `buf.len()` bytes starting at `offset`, returning the number read
    /// (0 only at/after EOF). `offset` is guaranteed `<= len()` by the caller.
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<usize, SinkError>;
}

/// Destination for the **receiver**: bytes are committed strictly in contiguous order
/// (the engine never calls with a gap), so an implementation may simply append.
pub trait FileSink {
    /// Bytes already durably held for this file — the resume point offered back to the
    /// sender in `FileAccept`. A fresh sink returns 0.
    fn existing_len(&self) -> u64;

    /// Commit `data` at `offset` (`offset == existing_len()` always holds). After a
    /// successful write `existing_len()` advances by `data.len()`.
    fn write_at(&mut self, offset: u64, data: &[u8]) -> Result<(), SinkError>;

    /// Finalize the file and return the SHA-256 of everything written. Called once the
    /// engine has committed `size` bytes; the receiver compares it to the expected
    /// digest (when known) for the integrity gate.
    fn finish(&mut self) -> Result<crate::Hash, SinkError>;
}

/// An in-memory [`FileSource`] wrapping owned bytes — used by tests and as the shape a
/// real mmap/`File` source mirrors.
#[derive(Clone, Debug)]
pub struct MemSource {
    bytes: Vec<u8>,
}

impl MemSource {
    /// Wrap `bytes` as a readable source.
    #[must_use]
    pub fn new(bytes: Vec<u8>) -> Self {
        Self { bytes }
    }
}

impl FileSource for MemSource {
    fn len(&self) -> u64 {
        self.bytes.len() as u64
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<usize, SinkError> {
        let start = usize::try_from(offset).map_err(|_| SinkError("offset overflow".into()))?;
        if start >= self.bytes.len() {
            return Ok(0);
        }
        let end = start.saturating_add(buf.len()).min(self.bytes.len());
        let src = self
            .bytes
            .get(start..end)
            .ok_or_else(|| SinkError("source range out of bounds".into()))?;
        let dst = buf
            .get_mut(..src.len())
            .ok_or_else(|| SinkError("dest range out of bounds".into()))?;
        dst.copy_from_slice(src);
        Ok(src.len())
    }
}

/// An in-memory [`FileSink`] accumulating received bytes — used by tests to assert the
/// reassembled content + hash without touching disk.
#[derive(Clone, Debug, Default)]
pub struct MemSink {
    bytes: Vec<u8>,
}

impl MemSink {
    /// A fresh empty sink (resume point 0).
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Pre-seed `prefix` bytes to simulate a partially-received file resuming.
    #[must_use]
    pub fn with_prefix(prefix: Vec<u8>) -> Self {
        Self { bytes: prefix }
    }

    /// The bytes received so far (the reassembled file once complete).
    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
}

impl FileSink for MemSink {
    fn existing_len(&self) -> u64 {
        self.bytes.len() as u64
    }

    fn write_at(&mut self, offset: u64, data: &[u8]) -> Result<(), SinkError> {
        if offset != self.existing_len() {
            return Err(SinkError(format!(
                "non-contiguous write at {offset}, have {}",
                self.bytes.len()
            )));
        }
        self.bytes.extend_from_slice(data);
        Ok(())
    }

    fn finish(&mut self) -> Result<crate::Hash, SinkError> {
        use sha2::{Digest, Sha256};
        let mut h = Sha256::new();
        h.update(&self.bytes);
        Ok(h.finalize().into())
    }
}

/// Convenience: SHA-256 of a byte slice (e.g. to compute a sender's expected digest).
#[must_use]
pub fn sha256(bytes: &[u8]) -> crate::Hash {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(bytes);
    h.finalize().into()
}
