//! Linux clipboard adapter — `wl-clipboard-rs`-backed
//! [`mouser_core::platform::Clipboard`].
//!
//! [`LinuxClipboard`] moves raw bytes to/from the **Wayland** clipboard for a given
//! [`ClipFormat`], addressing each format by its MIME type. It does **no**
//! canonicalization, hashing, dedup, or loop-prevention — per spec §7.7 that lives
//! in the engine; this is a thin byte mover for one format. X11-only sessions are
//! covered via XWayland on modern desktops; a native X11 backend can be added later
//! behind this same trait.
//!
//! ## Format mapping (spec §7.7 / Appendix C)
//! | `ClipFormat` | MIME type                  |
//! |--------------|----------------------------|
//! | `Utf8Text`   | `text/plain;charset=utf-8` |
//! | `Html`       | `text/html`                |
//! | `Rtf`        | `text/rtf`                 |
//! | `Png`        | `image/png`                |
//! | `UriList`    | `text/uri-list`            |
//!
//! All formats are carried as opaque bytes under their MIME type — the engine owns
//! every canonical form (CRLF→LF for text, LF-separated URIs, raw PNG, etc.).
//!
//! ## Change detection
//! Unlike macOS (`NSPasteboard.changeCount`) and Windows
//! (`GetClipboardSequenceNumber`), the Wayland data-control protocol exposes **no**
//! monotonic change counter. [`LinuxClipboard::change_token`] therefore returns a
//! content **hash** of the current `text/plain` representation: a poller compares
//! successive tokens and treats any difference as "the clipboard changed" (spec
//! §7.7). This catches the common text case; binary-only changes (e.g. an image
//! copy with no text rep) are caught by the engine's per-format offer hashing.
//!
//! ## Build
//! The Wayland backend is `#[cfg(target_os = "linux")]`; on other hosts only the
//! [`UNSUPPORTED`](crate::UNSUPPORTED) marker and this module's type *signatures*
//! (via the stub below) compile, so `cargo build -p platform-linux` succeeds
//! everywhere. The native code is exercised on Linux.

#![allow(clippy::module_name_repetitions)]

use mouser_core::platform::ClipFormat;

/// The MIME type a [`ClipFormat`] is carried as on the Wayland clipboard.
///
/// Shared by the native and stub builds so the mapping is verified on every host.
#[must_use]
pub fn mime_type(format: ClipFormat) -> &'static str {
    match format {
        ClipFormat::Utf8Text => "text/plain;charset=utf-8",
        ClipFormat::Html => "text/html",
        ClipFormat::Rtf => "text/rtf",
        ClipFormat::Png => "image/png",
        ClipFormat::UriList => "text/uri-list",
    }
}

#[cfg(target_os = "linux")]
mod imp {
    use std::io::Read;

    use mouser_core::platform::{ClipFormat, Clipboard, PlatformError, PlatformResult};
    use wl_clipboard_rs::copy::{MimeType as CopyMime, Options, Source};
    use wl_clipboard_rs::paste::{
        get_contents, ClipboardType, Error as PasteError, MimeType as PasteMime, Seat,
    };

    use super::mime_type;

    /// `wl-clipboard-rs`-backed [`Clipboard`] over the regular Wayland clipboard.
    ///
    /// Zero-sized: the clipboard is a compositor-global resource. Each `write` spawns
    /// the standard wl-clipboard serving handoff (the data is offered until another
    /// client takes ownership), matching how `wl-copy` behaves.
    #[derive(Debug, Default, Clone, Copy)]
    pub struct LinuxClipboard;

    impl LinuxClipboard {
        /// A clipboard adapter bound to the regular Wayland clipboard.
        #[must_use]
        pub fn new() -> Self {
            Self
        }

        /// A content token for change detection (Wayland has no native counter).
        ///
        /// Returns a 64-bit hash of the current `text/plain` clipboard contents, or
        /// `0` when the clipboard is empty / has no text. A poller compares
        /// successive tokens; any change means the clipboard changed (spec §7.7).
        /// Binary-only changes are caught by the engine's per-format offer hashing.
        #[must_use]
        pub fn change_token(&self) -> u64 {
            match read_mime(mime_type(ClipFormat::Utf8Text)) {
                Ok(Some(bytes)) => fnv1a(&bytes),
                _ => 0,
            }
        }
    }

    impl Clipboard for LinuxClipboard {
        fn read(&self, format: ClipFormat) -> PlatformResult<Option<Vec<u8>>> {
            read_mime(mime_type(format))
        }

        fn write(&self, format: ClipFormat, data: &[u8]) -> PlatformResult<()> {
            let mime = CopyMime::Specific(mime_type(format).to_owned());
            let source = Source::Bytes(data.to_vec().into_boxed_slice());
            Options::new()
                .copy(source, mime)
                .map_err(|e| -> PlatformError { Box::new(e) })
        }
    }

    /// Read the regular clipboard for one MIME type, mapping the "empty"/"no such
    /// type" family of errors to `Ok(None)` and real failures to `Err`.
    fn read_mime(mime: &str) -> PlatformResult<Option<Vec<u8>>> {
        match get_contents(
            ClipboardType::Regular,
            Seat::Unspecified,
            PasteMime::Specific(mime),
        ) {
            Ok((mut reader, _actual)) => {
                let mut buf = Vec::new();
                reader
                    .read_to_end(&mut buf)
                    .map_err(|e| -> PlatformError { Box::new(e) })?;
                Ok(Some(buf))
            }
            // These three are "effectively empty" per the crate's own docs.
            Err(PasteError::NoSeats)
            | Err(PasteError::ClipboardEmpty)
            | Err(PasteError::NoMimeType) => Ok(None),
            Err(e) => Err(Box::new(e)),
        }
    }

    /// FNV-1a 64-bit hash — a tiny, dependency-free content fingerprint for change
    /// detection. Not cryptographic; the engine owns the SHA-256 offer hashing.
    fn fnv1a(bytes: &[u8]) -> u64 {
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        for &b in bytes {
            h ^= u64::from(b);
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
        h
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn fnv1a_is_stable_and_distinguishing() {
            assert_eq!(fnv1a(b"hello"), fnv1a(b"hello"));
            assert_ne!(fnv1a(b"hello"), fnv1a(b"hellp"));
            assert_eq!(fnv1a(b""), 0xcbf2_9ce4_8422_2325);
        }

        #[test]
        fn copy_mime_is_owned_specific() {
            // The write path must hand wl-clipboard an owned, specific MIME type.
            let m = CopyMime::Specific(mime_type(ClipFormat::Png).to_owned());
            assert!(matches!(m, CopyMime::Specific(s) if s == "image/png"));
        }
    }
}

#[cfg(target_os = "linux")]
pub use imp::LinuxClipboard;

/// Non-Linux stub of the clipboard adapter so the crate compiles on macOS / Windows.
///
/// It implements the [`Clipboard`](mouser_core::platform::Clipboard) trait by
/// returning a typed [`Unsupported`] error for every operation, and a `change_token`
/// of `0`. This keeps the type and its trait impl present off-target (so dependent
/// code type-checks everywhere) while the real Wayland backend is Linux-only.
#[cfg(not(target_os = "linux"))]
#[derive(Debug, Default, Clone, Copy)]
pub struct LinuxClipboard;

#[cfg(not(target_os = "linux"))]
impl LinuxClipboard {
    /// A stub clipboard adapter (no Wayland backend off Linux).
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    /// Always `0` off-target: there is no Wayland clipboard to fingerprint.
    #[must_use]
    pub fn change_token(&self) -> u64 {
        0
    }
}

#[cfg(not(target_os = "linux"))]
impl mouser_core::platform::Clipboard for LinuxClipboard {
    fn read(&self, _format: ClipFormat) -> mouser_core::platform::PlatformResult<Option<Vec<u8>>> {
        Err(Box::new(Unsupported))
    }

    fn write(
        &self,
        _format: ClipFormat,
        _data: &[u8],
    ) -> mouser_core::platform::PlatformResult<()> {
        Err(Box::new(Unsupported))
    }
}

/// The Linux clipboard backend was called on a non-Linux host (stub build).
#[cfg(not(target_os = "linux"))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Unsupported;

#[cfg(not(target_os = "linux"))]
impl std::fmt::Display for Unsupported {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "the Linux Wayland clipboard backend is only available on Linux"
        )
    }
}

#[cfg(not(target_os = "linux"))]
impl std::error::Error for Unsupported {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mime_mapping_is_total_and_distinct() {
        let formats = [
            ClipFormat::Utf8Text,
            ClipFormat::Html,
            ClipFormat::Rtf,
            ClipFormat::Png,
            ClipFormat::UriList,
        ];
        let mimes: Vec<&str> = formats.iter().copied().map(mime_type).collect();
        for m in &mimes {
            assert!(m.contains('/'), "mime {m} is not a type/subtype");
        }
        // All five map to distinct MIME types.
        let mut sorted = mimes.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), formats.len());
    }

    #[cfg(not(target_os = "linux"))]
    #[test]
    fn stub_reports_unsupported() {
        use mouser_core::platform::Clipboard;
        let cb = LinuxClipboard::new();
        let err = cb.read(ClipFormat::Utf8Text).expect_err("stub must error");
        assert!(err.downcast_ref::<Unsupported>().is_some());
        assert_eq!(cb.change_token(), 0);
    }
}
