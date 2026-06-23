//! macOS clipboard adapter — `NSPasteboard`-backed [`mouser_core::platform::Clipboard`].
//!
//! [`MacClipboard`] moves raw bytes to/from the **general** `NSPasteboard` for a
//! given [`ClipFormat`]. It does **no** canonicalization, hashing, dedup, or
//! loop-prevention — per spec §7.7 that all lives in the engine; the adapter is a
//! thin byte mover for one format at a time.
//!
//! ## Format mapping (spec §7.7 / Appendix C)
//! | `ClipFormat` | `NSPasteboard` UTI                                  |
//! |--------------|----------------------------------------------------|
//! | `Utf8Text`   | `NSPasteboardTypeString` (`public.utf8-plain-text`)|
//! | `Html`       | `NSPasteboardTypeHTML` (`public.html`)             |
//! | `Rtf`        | `NSPasteboardTypeRTF` (`public.rtf`)              |
//! | `Png`        | `NSPasteboardTypePNG` (`public.png`)             |
//! | `UriList`    | `NSPasteboardTypeFileURL` (`public.file-url`)     |
//!
//! `Utf8Text` round-trips as a string (`setString:forType:` /
//! `stringForType:`); the binary formats round-trip as `NSData`
//! (`setData:forType:` / `dataForType:`). `UriList` bridges the engine's
//! LF-separated `file://` URI list to native `public.file-url` pasteboard items,
//! so Finder and other AppKit consumers can interoperate.
//!
//! ## Change detection
//! [`MacClipboard::change_count`] exposes `NSPasteboard.changeCount`, a counter the
//! OS bumps on **every** modification (local or remote). A poller compares it to the
//! last seen value to detect that the local clipboard changed and a fresh
//! `ClipboardOffer` may be due (spec §7.7). It does not say *what* changed.
//!
//! ## `unsafe`
//! The pasteboard accessors (`setData`/`setString`/`dataForType`/`stringForType`)
//! are safe in this `objc2` binding. The only `unsafe` here is reading the extern
//! `NSPasteboardType*` constant statics in [`pasteboard_type`] and
//! [`read_uri_list`], each carrying a `// SAFETY:` note. The rest of the crate stays
//! wrapper-only (see `lib.rs`).

use mouser_core::platform::{ClipFormat, Clipboard, PlatformError, PlatformResult};
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_app_kit::{
    NSPasteboard, NSPasteboardType, NSPasteboardTypeFileURL, NSPasteboardTypeHTML,
    NSPasteboardTypePNG, NSPasteboardTypeRTF, NSPasteboardTypeString, NSPasteboardWriting,
};
use objc2_foundation::{NSArray, NSData, NSString, NSURL};

/// `NSPasteboard`-backed [`Clipboard`] over the general pasteboard.
///
/// Zero-sized: the general pasteboard is a process-wide singleton, so there is no
/// per-instance state. `change_count` reads live OS state on each call.
#[derive(Debug, Default, Clone, Copy)]
pub struct MacClipboard;

impl MacClipboard {
    /// A clipboard adapter bound to the general pasteboard.
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    /// The current `NSPasteboard.changeCount`.
    ///
    /// The OS increments this on every pasteboard mutation. A poller stores the
    /// previous value and treats any increase as "the clipboard changed" (spec
    /// §7.7 change detection); it conveys nothing about the new contents.
    #[must_use]
    pub fn change_count(&self) -> i64 {
        let pb = NSPasteboard::generalPasteboard();
        // `changeCount` returns `NSInteger` (`isize`); widen to a stable i64.
        pb.changeCount() as i64
    }
}

/// Whether a [`ClipFormat`] is carried as an `NSString` (text) vs raw `NSData`.
///
/// Only `Utf8Text` uses the string path; binary formats use the data path, and
/// `UriList` has a native file-URL item path.
fn is_string_format(format: ClipFormat) -> bool {
    matches!(format, ClipFormat::Utf8Text)
}

/// Resolve a [`ClipFormat`] to its `NSPasteboard` UTI string.
///
/// The AppKit-provided UTIs are read from their extern statics.
fn pasteboard_type(format: ClipFormat) -> Retained<NSString> {
    match format {
        // SAFETY: `NSPasteboardType*` are CoreFoundation/AppKit extern constant
        // `NSString`s; reading them is the documented usage. `NSPasteboardType` is
        // a typed alias for `NSString`, so the deref yields a live `&NSString` we
        // copy into an owned `Retained` to drop the borrow on the static.
        ClipFormat::Utf8Text => copy_type(unsafe { NSPasteboardTypeString }),
        // SAFETY: as above.
        ClipFormat::Html => copy_type(unsafe { NSPasteboardTypeHTML }),
        // SAFETY: as above.
        ClipFormat::Rtf => copy_type(unsafe { NSPasteboardTypeRTF }),
        // SAFETY: as above.
        ClipFormat::Png => copy_type(unsafe { NSPasteboardTypePNG }),
        // SAFETY: as above.
        ClipFormat::UriList => copy_type(unsafe { NSPasteboardTypeFileURL }),
    }
}

/// Copy an extern `NSPasteboardType` static into an owned `Retained<NSString>`.
///
/// Keeps the `unsafe` static read scoped to [`pasteboard_type`] and hands callers a
/// plain owned string with no lifetime tied to the global.
fn copy_type(t: &NSPasteboardType) -> Retained<NSString> {
    NSString::from_str(&t.to_string())
}

impl Clipboard for MacClipboard {
    fn read(&self, format: ClipFormat) -> PlatformResult<Option<Vec<u8>>> {
        let pb = NSPasteboard::generalPasteboard();
        if matches!(format, ClipFormat::UriList) {
            return Ok(read_uri_list(&pb));
        }

        let ty = pasteboard_type(format);

        if is_string_format(format) {
            // `stringForType:` returns an owned `Option<Retained<NSString>>` (objc2
            // retains it) or `None` when the type is absent. Safe in this objc2 binding.
            let s = pb.stringForType(&ty);
            Ok(s.map(|s| s.to_string().into_bytes()))
        } else {
            // `dataForType:` returns an owned `Option<Retained<NSData>>` or `None`.
            let data = pb.dataForType(&ty);
            Ok(data.map(|d| d.to_vec()))
        }
    }

    fn write(&self, format: ClipFormat, data: &[u8]) -> PlatformResult<()> {
        let pb = NSPasteboard::generalPasteboard();

        if matches!(format, ClipFormat::UriList) {
            let urls = uri_list_to_file_urls(data)?;
            pb.clearContents();
            return write_file_urls(&pb, &urls, format);
        }

        let ty = pasteboard_type(format);

        let ok = if is_string_format(format) {
            // Text must be valid UTF-8 before `clearContents`, so a mis-tagged binary
            // payload can't wipe the existing pasteboard and then fail to write.
            let s = std::str::from_utf8(data).map_err(|e| -> PlatformError { Box::new(e) })?;
            let ns = NSString::from_str(s);
            pb.clearContents();
            // `setString:forType:` copies the bytes into the pasteboard and returns
            // whether the write succeeded. Safe in this objc2 binding.
            pb.setString_forType(&ns, &ty)
        } else {
            let nsdata = NSData::with_bytes(data);
            pb.clearContents();
            // `setData:forType:` copies the bytes and returns whether it succeeded.
            pb.setData_forType(Some(&nsdata), &ty)
        };

        if ok {
            Ok(())
        } else {
            Err(Box::new(ClipboardWriteFailed(format)))
        }
    }
}

/// Read native `public.file-url` pasteboard items as an LF-separated URI list.
fn read_uri_list(pb: &NSPasteboard) -> Option<Vec<u8>> {
    let mut uris = Vec::new();
    let items = pb.pasteboardItems()?;
    for item in items.iter() {
        // SAFETY: `item` is a live pasteboard item, and `NSPasteboardTypeFileURL` is
        // an AppKit extern NSString constant for `public.file-url`. `stringForType:`
        // returns an owned string or nil and imposes no additional lifetime on us.
        if let Some(uri) = unsafe { item.stringForType(NSPasteboardTypeFileURL) } {
            let uri = uri.to_string();
            if !uri.is_empty() {
                uris.push(uri);
            }
        }
    }
    if uris.is_empty() {
        None
    } else {
        Some(uris.join("\n").into_bytes())
    }
}

/// Parse a `text/uri-list` payload into native file URLs.
fn uri_list_to_file_urls(data: &[u8]) -> PlatformResult<Vec<Retained<NSURL>>> {
    let text = std::str::from_utf8(data).map_err(|e| -> PlatformError { Box::new(e) })?;
    let urls = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .filter_map(file_url_from_uri)
        .collect();
    Ok(urls)
}

/// Convert one `file://` URI line to an `NSURL`, skipping non-file URI entries.
fn file_url_from_uri(uri: &str) -> Option<Retained<NSURL>> {
    let ns = NSString::from_str(uri);
    let url = NSURL::URLWithString(&ns)?;
    url.isFileURL().then_some(url)
}

/// Write native file URL pasteboard objects.
fn write_file_urls(
    pb: &NSPasteboard,
    urls: &[Retained<NSURL>],
    format: ClipFormat,
) -> PlatformResult<()> {
    if urls.is_empty() {
        return Ok(());
    }
    let writers: Vec<&ProtocolObject<dyn NSPasteboardWriting>> = urls
        .iter()
        .map(|u| ProtocolObject::from_ref(&**u))
        .collect();
    let array: Retained<NSArray<ProtocolObject<dyn NSPasteboardWriting>>> =
        NSArray::from_slice(&writers);
    if pb.writeObjects(&array) {
        Ok(())
    } else {
        Err(Box::new(ClipboardWriteFailed(format)))
    }
}

/// An `NSPasteboard` `setData:`/`setString:` returned `NO` for `format`.
///
/// AppKit reports a refused write only as a boolean; the most common cause is the
/// pasteboard owner being changed concurrently between `clearContents` and the set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClipboardWriteFailed(pub ClipFormat);

impl std::fmt::Display for ClipboardWriteFailed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "NSPasteboard refused the write for {:?}", self.0)
    }
}

impl std::error::Error for ClipboardWriteFailed {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// The general pasteboard is a process-wide OS singleton; `cargo test` runs
    /// tests in parallel threads, so any test that *mutates* it must hold this lock
    /// to avoid another test clobbering the contents between its write and read.
    static PASTEBOARD: Mutex<()> = Mutex::new(());

    /// Acquire [`PASTEBOARD`], tolerating poisoning so one failed assertion in a
    /// guarded test doesn't cascade into spurious failures in the others.
    fn pasteboard_guard() -> std::sync::MutexGuard<'static, ()> {
        PASTEBOARD.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Every `ClipFormat` resolves to a non-empty UTI, and `Utf8Text` is the only
    /// string-carried format.
    #[test]
    fn format_mapping_is_total() {
        for f in [
            ClipFormat::Utf8Text,
            ClipFormat::Html,
            ClipFormat::Rtf,
            ClipFormat::Png,
            ClipFormat::UriList,
        ] {
            assert!(!pasteboard_type(f).to_string().is_empty());
        }
        assert!(is_string_format(ClipFormat::Utf8Text));
        assert!(!is_string_format(ClipFormat::Png));
        assert!(!is_string_format(ClipFormat::UriList));
    }

    /// `UriList` maps to native `public.file-url`.
    #[test]
    fn uri_list_uses_native_file_url_uti() {
        assert_eq!(
            pasteboard_type(ClipFormat::UriList).to_string(),
            "public.file-url"
        );
    }

    #[test]
    fn uri_list_parses_only_file_urls() {
        let urls = uri_list_to_file_urls(
            b"# comment\nhttps://example.com/x\nfile:///Users/me/a%20b.txt\n\nfile:///tmp/c.txt",
        )
        .expect("valid utf8");
        assert_eq!(urls.len(), 2);
    }

    /// Round-trip UTF-8 text through the real general pasteboard. Requires a window
    /// server session; on a headless CI box the write is refused, in which case we
    /// accept the typed error rather than failing (the path is still exercised).
    #[test]
    fn utf8_text_roundtrip_or_no_session() {
        let _guard = pasteboard_guard();
        let cb = MacClipboard::new();
        let payload = "héllo, clip — ✓\nsecond line";
        match cb.write(ClipFormat::Utf8Text, payload.as_bytes()) {
            Ok(()) => {
                let got = cb
                    .read(ClipFormat::Utf8Text)
                    .expect("read ok")
                    .expect("present after write");
                assert_eq!(got, payload.as_bytes());
            }
            Err(e) => {
                // Headless: no pasteboard session. Confirm it's the typed refusal.
                assert!(e.downcast_ref::<ClipboardWriteFailed>().is_some());
            }
        }
    }

    /// PNG bytes round-trip as opaque `NSData` (or the headless refusal).
    #[test]
    fn png_bytes_roundtrip_or_no_session() {
        let _guard = pasteboard_guard();
        let cb = MacClipboard::new();
        // A minimal valid PNG signature + IHDR-ish bytes; content is opaque to us.
        let payload: &[u8] = &[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x01];
        match cb.write(ClipFormat::Png, payload) {
            Ok(()) => {
                let got = cb.read(ClipFormat::Png).expect("read ok").expect("present");
                assert_eq!(got, payload);
            }
            Err(e) => assert!(e.downcast_ref::<ClipboardWriteFailed>().is_some()),
        }
    }

    /// Writing non-UTF-8 bytes as `Utf8Text` is rejected with a `Utf8Error`, not a
    /// silent corruption.
    #[test]
    fn non_utf8_text_write_is_rejected() {
        let cb = MacClipboard::new();
        let bad: &[u8] = &[0xff, 0xfe, 0x00];
        let err = cb
            .write(ClipFormat::Utf8Text, bad)
            .expect_err("must reject");
        assert!(err.downcast_ref::<std::str::Utf8Error>().is_some());
    }

    /// A bad UTF-8 `Utf8Text` write must fail before `clearContents`, preserving the
    /// previous pasteboard value when a WindowServer session is available.
    #[test]
    fn non_utf8_text_write_preserves_existing_pasteboard() {
        let _guard = pasteboard_guard();
        let cb = MacClipboard::new();
        let original = b"mouser clipboard sentinel\n";
        match cb.write(ClipFormat::Utf8Text, original) {
            Ok(()) => {
                let bad: &[u8] = &[0xff, 0xfe, 0x00];
                let err = cb
                    .write(ClipFormat::Utf8Text, bad)
                    .expect_err("must reject");
                assert!(err.downcast_ref::<std::str::Utf8Error>().is_some());
                let got = cb
                    .read(ClipFormat::Utf8Text)
                    .expect("read ok")
                    .expect("existing value remains present");
                assert_eq!(got, original);
            }
            Err(e) => {
                // Headless: no pasteboard session. Confirm it's the typed refusal.
                assert!(e.downcast_ref::<ClipboardWriteFailed>().is_some());
            }
        }
    }

    /// `change_count` is monotonic across a write (when a session exists). Without a
    /// session the count is stable; either way it must not decrease.
    #[test]
    fn change_count_does_not_decrease_across_write() {
        let _guard = pasteboard_guard();
        let cb = MacClipboard::new();
        let before = cb.change_count();
        let _ = cb.write(ClipFormat::Utf8Text, b"x");
        let after = cb.change_count();
        assert!(after >= before);
    }
}
