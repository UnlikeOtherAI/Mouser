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
//! content **hash** over a stable ordered concatenation of every supported MIME
//! representation that can be read (`text/plain`, HTML, RTF, PNG, URI list). A
//! poller compares successive tokens and treats any difference as "the clipboard
//! changed" (spec §7.7), including binary-only image/file changes.
//!
//! ## Build
//! The Wayland backend is `#[cfg(target_os = "linux")]`; on other hosts only the
//! [`UNSUPPORTED`](crate::UNSUPPORTED) marker and this module's type *signatures*
//! (via the stub below) compile, so `cargo build -p platform-linux` succeeds
//! everywhere. The native code is exercised on Linux.

#![allow(clippy::module_name_repetitions)]

use mouser_core::platform::ClipFormat;

#[cfg(any(test, target_os = "linux"))]
const TOKEN_FORMATS: [ClipFormat; 5] = [
    ClipFormat::Utf8Text,
    ClipFormat::Html,
    ClipFormat::Rtf,
    ClipFormat::Png,
    ClipFormat::UriList,
];

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

    use super::{change_token_from_reader, mime_type};

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
        /// Returns a 64-bit hash over every supported MIME representation that can
        /// be read. A poller compares successive tokens; any change means the
        /// clipboard changed (spec §7.7), including binary-only image/file changes.
        #[must_use]
        pub fn change_token(&self) -> u64 {
            change_token_from_reader(|format| match read_mime(mime_type(format)) {
                Ok(Some(bytes)) => Some(bytes),
                _ => None,
            })
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

    #[cfg(test)]
    mod tests {
        use super::super::fnv1a;
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

/// Build a stable token from the readable clipboard representations.
#[cfg(any(test, target_os = "linux"))]
fn change_token_from_reader<F>(mut read: F) -> u64
where
    F: FnMut(ClipFormat) -> Option<Vec<u8>>,
{
    let mut h = FNV_OFFSET;
    for format in TOKEN_FORMATS {
        fnv1a_update(&mut h, mime_type(format).as_bytes());
        fnv1a_update(&mut h, &[0]);
        if let Some(bytes) = read(format) {
            fnv1a_update(&mut h, &[1]);
            fnv1a_update(&mut h, &(bytes.len() as u64).to_le_bytes());
            fnv1a_update(&mut h, &bytes);
        } else {
            fnv1a_update(&mut h, &[0]);
        }
    }
    h
}

#[cfg(any(test, target_os = "linux"))]
const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
#[cfg(any(test, target_os = "linux"))]
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

/// FNV-1a 64-bit hash — a tiny, dependency-free content fingerprint for change
/// detection. Not cryptographic; the engine owns the SHA-256 offer hashing.
#[cfg(all(test, target_os = "linux"))]
fn fnv1a(bytes: &[u8]) -> u64 {
    let mut h = FNV_OFFSET;
    fnv1a_update(&mut h, bytes);
    h
}

#[cfg(any(test, target_os = "linux"))]
fn fnv1a_update(h: &mut u64, bytes: &[u8]) {
    for &b in bytes {
        *h ^= u64::from(b);
        *h = h.wrapping_mul(FNV_PRIME);
    }
}

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

    #[test]
    fn change_token_includes_binary_only_representations() {
        let text_only = change_token_from_reader(|format| {
            matches!(format, ClipFormat::Utf8Text).then(|| b"same".to_vec())
        });
        let with_png = change_token_from_reader(|format| match format {
            ClipFormat::Utf8Text => Some(b"same".to_vec()),
            ClipFormat::Png => Some(vec![0x89, b'P', b'N', b'G']),
            _ => None,
        });
        let png_only = change_token_from_reader(|format| {
            matches!(format, ClipFormat::Png).then(|| vec![0x89, b'P', b'N', b'G'])
        });

        assert_ne!(text_only, with_png);
        assert_ne!(text_only, png_only);
        assert_eq!(
            with_png,
            change_token_from_reader(|format| match format {
                ClipFormat::Utf8Text => Some(b"same".to_vec()),
                ClipFormat::Png => Some(vec![0x89, b'P', b'N', b'G']),
                _ => None,
            })
        );
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
