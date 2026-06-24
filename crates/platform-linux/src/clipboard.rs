//! Linux clipboard adapter — Wayland and X11-backed
//! [`mouser_core::platform::Clipboard`].
//!
//! [`LinuxClipboard`] moves raw bytes to/from the session clipboard for a given
//! [`ClipFormat`], addressing each format by its MIME type where the compositor or
//! selection protocol supports it. It does **no**
//! canonicalization, hashing, dedup, or loop-prevention — per spec §7.7 that lives
//! in the engine; this is a thin byte mover for one format. Runtime selection keeps
//! the existing Wayland path for Wayland sessions and uses a native X11 selection
//! backend for pure-X11 sessions.
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
//! The native backends are `#[cfg(target_os = "linux")]`; on other hosts only the
//! [`UNSUPPORTED`](crate::UNSUPPORTED) marker and this module's type *signatures*
//! (via the stub below) compile, so `cargo build -p platform-linux` succeeds
//! everywhere. The native code is exercised on Linux. The X11 backend uses the
//! `UTF8_STRING` target for [`ClipFormat::Utf8Text`] and MIME atoms for rich and
//! binary formats.

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

/// The MIME type a [`ClipFormat`] is carried as on the Linux clipboard.
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
    use std::sync::{Mutex, MutexGuard, PoisonError};
    use std::time::Duration;

    use mouser_core::platform::{
        ClipFormat, Clipboard as CoreClipboard, PlatformError, PlatformResult,
    };
    use wl_clipboard_rs::copy::{MimeType as CopyMime, Options, Source};
    use wl_clipboard_rs::paste::{
        get_contents, ClipboardType, Error as PasteError, MimeType as PasteMime, Seat,
    };
    use x11_clipboard::{Atom, Clipboard as X11Clipboard};

    use super::{change_token_from_reader, mime_type};

    const X11_TIMEOUT: Duration = Duration::from_secs(3);

    /// Clipboard adapter selected for the current Linux graphical session.
    pub struct LinuxClipboard {
        backend: Backend,
    }

    enum Backend {
        Wayland,
        X11(Box<Mutex<X11Clipboard>>),
        Unavailable(ClipboardUnavailable),
    }

    impl Default for LinuxClipboard {
        fn default() -> Self {
            Self::new()
        }
    }

    impl LinuxClipboard {
        /// A clipboard adapter bound to the current session type.
        #[must_use]
        pub fn new() -> Self {
            Self {
                backend: selected_backend(),
            }
        }

        /// A content token for change detection (Wayland has no native counter).
        ///
        /// Returns a 64-bit hash over every supported MIME representation that can
        /// be read. A poller compares successive tokens; any change means the
        /// clipboard changed (spec §7.7), including binary-only image/file changes.
        #[must_use]
        pub fn change_token(&self) -> u64 {
            change_token_from_reader(|format| match self.read_backend(format) {
                Ok(Some(bytes)) => Some(bytes),
                _ => None,
            })
        }

        fn read_backend(&self, format: ClipFormat) -> PlatformResult<Option<Vec<u8>>> {
            match &self.backend {
                Backend::Wayland => read_wayland_mime(mime_type(format)),
                Backend::X11(clipboard) => read_x11(clipboard.as_ref(), format),
                Backend::Unavailable(e) => Err(Box::new(e.clone())),
            }
        }

        fn write_backend(&self, format: ClipFormat, data: &[u8]) -> PlatformResult<()> {
            match &self.backend {
                Backend::Wayland => write_wayland_mime(mime_type(format), data),
                Backend::X11(clipboard) => write_x11(clipboard.as_ref(), format, data),
                Backend::Unavailable(e) => Err(Box::new(e.clone())),
            }
        }
    }

    impl CoreClipboard for LinuxClipboard {
        fn change_token(&self) -> PlatformResult<u64> {
            Ok(LinuxClipboard::change_token(self))
        }

        fn read(&self, format: ClipFormat) -> PlatformResult<Option<Vec<u8>>> {
            self.read_backend(format)
        }

        fn write(&self, format: ClipFormat, data: &[u8]) -> PlatformResult<()> {
            self.write_backend(format, data)
        }
    }

    fn selected_backend() -> Backend {
        if use_x11_backend() {
            match X11Clipboard::new() {
                Ok(clipboard) => Backend::X11(Box::new(Mutex::new(clipboard))),
                Err(e) => Backend::Unavailable(ClipboardUnavailable {
                    backend: "X11",
                    message: e.to_string(),
                }),
            }
        } else {
            Backend::Wayland
        }
    }

    fn use_x11_backend() -> bool {
        use_x11_backend_for(
            std::env::var("XDG_SESSION_TYPE").ok().as_deref(),
            std::env::var_os("DISPLAY").is_some(),
            std::env::var_os("WAYLAND_DISPLAY").is_some(),
        )
    }

    fn use_x11_backend_for(session: Option<&str>, display: bool, wayland: bool) -> bool {
        match session.map(str::to_ascii_lowercase).as_deref() {
            Some("x11") => true,
            Some("wayland") => false,
            _ => display && !wayland,
        }
    }

    /// Read the regular clipboard for one MIME type, mapping the "empty"/"no such
    /// type" family of errors to `Ok(None)` and real failures to `Err`.
    fn read_wayland_mime(mime: &str) -> PlatformResult<Option<Vec<u8>>> {
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

    fn write_wayland_mime(mime: &str, data: &[u8]) -> PlatformResult<()> {
        let mime = CopyMime::Specific(mime.to_owned());
        let source = Source::Bytes(data.to_vec().into_boxed_slice());
        Options::new()
            .copy(source, mime)
            .map_err(|e| -> PlatformError { Box::new(e) })
    }

    fn read_x11(
        clipboard: &Mutex<X11Clipboard>,
        format: ClipFormat,
    ) -> PlatformResult<Option<Vec<u8>>> {
        let clipboard = lock_recover(clipboard);
        let target = x11_target(&clipboard, format)?;
        let bytes = clipboard
            .load(
                clipboard.getter.atoms.clipboard,
                target,
                clipboard.getter.atoms.property,
                X11_TIMEOUT,
            )
            .map_err(|e| -> PlatformError { Box::new(e) })?;
        if bytes.is_empty() {
            Ok(None)
        } else {
            Ok(Some(bytes))
        }
    }

    fn write_x11(
        clipboard: &Mutex<X11Clipboard>,
        format: ClipFormat,
        data: &[u8],
    ) -> PlatformResult<()> {
        let clipboard = lock_recover(clipboard);
        let target = x11_target(&clipboard, format)?;
        clipboard
            .store(clipboard.setter.atoms.clipboard, target, data.to_vec())
            .map_err(|e| -> PlatformError { Box::new(e) })
    }

    fn x11_target(clipboard: &X11Clipboard, format: ClipFormat) -> PlatformResult<Atom> {
        match format {
            ClipFormat::Utf8Text => Ok(clipboard.getter.atoms.utf8_string),
            other => clipboard
                .getter
                .get_atom(mime_type(other))
                .map_err(|e| -> PlatformError { Box::new(e) }),
        }
    }

    fn lock_recover<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
        m.lock().unwrap_or_else(PoisonError::into_inner)
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    struct ClipboardUnavailable {
        backend: &'static str,
        message: String,
    }

    impl std::fmt::Display for ClipboardUnavailable {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(
                f,
                "{} clipboard backend unavailable: {}",
                self.backend, self.message
            )
        }
    }

    impl std::error::Error for ClipboardUnavailable {}

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

        #[test]
        fn session_type_selects_x11_only_for_x11() {
            assert!(use_x11_backend_for(Some("x11"), true, true));
            assert!(!use_x11_backend_for(Some("wayland"), true, true));
            assert!(use_x11_backend_for(None, true, false));
            assert!(!use_x11_backend_for(None, true, true));
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
/// code type-checks everywhere) while the real backends are Linux-only.
#[cfg(not(target_os = "linux"))]
#[derive(Debug, Default, Clone, Copy)]
pub struct LinuxClipboard;

#[cfg(not(target_os = "linux"))]
impl LinuxClipboard {
    /// A stub clipboard adapter (no Linux clipboard backend off Linux).
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    /// Always `0` off-target: there is no Linux clipboard to fingerprint.
    #[must_use]
    pub fn change_token(&self) -> u64 {
        0
    }
}

#[cfg(not(target_os = "linux"))]
impl mouser_core::platform::Clipboard for LinuxClipboard {
    fn change_token(&self) -> mouser_core::platform::PlatformResult<u64> {
        Ok(0)
    }

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
        write!(f, "the Linux clipboard backend is only available on Linux")
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
