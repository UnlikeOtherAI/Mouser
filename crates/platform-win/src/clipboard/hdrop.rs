//! `CF_HDROP` ↔ `text/uri-list` bridge for the Windows clipboard adapter.
//!
//! The wire carries files as a `text/uri-list` (LF-separated `file://` URIs, spec
//! §7.7); the native Windows clipboard carries them as a `CF_HDROP` — a `DROPFILES`
//! header followed by a double-NUL-terminated list of wide (UTF-16) filesystem
//! paths. This module converts between the two:
//!
//! - [`hdrop_bytes_to_uri_list`]: read a clipboard `CF_HDROP` handle via
//!   `DragQueryFileW` and emit `file://` URIs, LF-separated, no trailing blank line
//!   (the canonical form the engine expects, spec §7.7).
//! - [`uri_list_to_hdrop_bytes`]: parse a `text/uri-list`, take the `file://`
//!   entries, and build the in-memory `CF_HDROP` blob for `SetClipboardData`.
//!
//! Path ↔ `file://` URI conversion is intentionally minimal: enough to round-trip
//! ordinary local paths (percent-encoding only the characters that must be escaped).
//! Non-`file:` URIs are skipped on write — `CF_HDROP` is filesystem paths only.

use std::path::PathBuf;

use windows::Win32::Foundation::HANDLE;
use windows::Win32::UI::Shell::{DragQueryFileW, DROPFILES, HDROP};

use super::ClipboardError;
use mouser_core::platform::{PlatformError, PlatformResult};

/// Read a `CF_HDROP` clipboard handle and render its files as a `text/uri-list`.
///
/// Uses `DragQueryFileW` to enumerate the drop list (first call with index
/// `0xFFFFFFFF` returns the count, then one query per file), converts each path to a
/// `file://` URI, and joins them with LF (no trailing newline).
pub(super) fn hdrop_bytes_to_uri_list(handle: HANDLE) -> PlatformResult<Vec<u8>> {
    let hdrop = HDROP(handle.0);
    // SAFETY: `0xFFFFFFFF` asks DragQueryFileW for the file count; with no buffer it
    // only reads the count from the (clipboard-owned, still-open) drop handle.
    let count = unsafe { DragQueryFileW(hdrop, 0xFFFF_FFFF, None) };

    let mut uris: Vec<String> = Vec::with_capacity(count as usize);
    for i in 0..count {
        // SAFETY: query the length (chars, excl. NUL) for file `i` with no buffer.
        let len = unsafe { DragQueryFileW(hdrop, i, None) };
        if len == 0 {
            continue;
        }
        // +1 for the NUL terminator DragQueryFileW writes.
        let mut buf = vec![0u16; len as usize + 1];
        // SAFETY: `buf` has room for `len` chars + NUL; DragQueryFileW fills it and
        // returns the chars written (excl. NUL).
        let written = unsafe { DragQueryFileW(hdrop, i, Some(buf.as_mut_slice())) };
        if written == 0 {
            continue;
        }
        buf.truncate(written as usize);
        let path = String::from_utf16_lossy(&buf);
        uris.push(path_to_file_uri(&path));
    }
    Ok(uris.join("\n").into_bytes())
}

/// Parse a `text/uri-list` payload and build a `CF_HDROP` `HGLOBAL` blob.
///
/// Comment lines (`#…`) and non-`file:` URIs are ignored — `CF_HDROP` is local
/// filesystem paths only. The blob layout is: `DROPFILES { pFiles = sizeof header,
/// fWide = TRUE }` then each path as a NUL-terminated wide string, then a final
/// extra NUL terminating the list.
pub(super) fn uri_list_to_hdrop_bytes(data: &[u8]) -> PlatformResult<Vec<u8>> {
    let text = std::str::from_utf8(data)
        .map_err(|_| -> PlatformError { Box::new(ClipboardError::InvalidUriList) })?;

    let paths: Vec<PathBuf> = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .filter_map(file_uri_to_path)
        .collect();

    // Header is a DROPFILES struct; the file list begins immediately after it.
    let header_size = std::mem::size_of::<DROPFILES>();
    let mut header = DROPFILES {
        pFiles: header_size as u32,
        pt: Default::default(),
        fNC: false.into(),
        fWide: true.into(),
    };

    let mut blob = Vec::new();
    // Serialize the header struct as raw bytes.
    // SAFETY: `DROPFILES` is `#[repr(C)]` plain-old-data with no padding we read back;
    // we view its bytes only to copy them into the blob (no aliasing, no escape).
    let header_bytes = unsafe {
        std::slice::from_raw_parts(std::ptr::addr_of_mut!(header).cast::<u8>(), header_size)
    };
    blob.extend_from_slice(header_bytes);

    for p in &paths {
        for u in p.as_os_str().to_string_lossy().encode_utf16() {
            blob.extend_from_slice(&u.to_le_bytes());
        }
        blob.extend_from_slice(&0u16.to_le_bytes()); // NUL after each path
    }
    blob.extend_from_slice(&0u16.to_le_bytes()); // final list terminator

    // Return the raw blob; the caller (`write`) copies it into a `GMEM_MOVEABLE`
    // `HGLOBAL` and hands ownership to `SetClipboardData`.
    Ok(blob)
}

/// Convert a Windows path string to a `file://` URI (minimal percent-encoding).
///
/// Backslashes become forward slashes; a drive-letter path gets the conventional
/// extra slash (`file:///C:/…`). Only characters that must be escaped in a URI path
/// are percent-encoded; ASCII path characters pass through.
fn path_to_file_uri(path: &str) -> String {
    let forward = path.replace('\\', "/");
    let mut encoded = String::with_capacity(forward.len() + 8);
    for ch in forward.chars() {
        match ch {
            'A'..='Z' | 'a'..='z' | '0'..='9' | '/' | '-' | '_' | '.' | '~' | ':' => {
                encoded.push(ch)
            }
            _ => {
                let mut b = [0u8; 4];
                for byte in ch.encode_utf8(&mut b).as_bytes() {
                    encoded.push('%');
                    encoded.push_str(&format!("{byte:02X}"));
                }
            }
        }
    }
    // A path starting with a drive letter or UNC root needs `file:///` / `file://`.
    if encoded.starts_with('/') {
        format!("file://{encoded}")
    } else {
        format!("file:///{encoded}")
    }
}

/// Convert a `file://` URI to a Windows [`PathBuf`], or `None` for non-`file:` URIs.
///
/// Percent-decodes, strips the `file://` (and the leading slash before a drive
/// letter), and restores backslashes.
fn file_uri_to_path(uri: &str) -> Option<PathBuf> {
    let rest = uri.strip_prefix("file://")?;
    // `file:///C:/x` → `/C:/x`; drop the leading slash before a `C:` drive spec.
    let trimmed = match rest.strip_prefix('/') {
        Some(after) if is_drive_prefixed(after) => after,
        _ => rest,
    };
    let decoded = percent_decode(trimmed);
    Some(PathBuf::from(decoded.replace('/', "\\")))
}

/// Whether `s` begins with a `C:`-style drive spec (`<letter>:`).
fn is_drive_prefixed(s: &str) -> bool {
    let mut it = s.chars();
    matches!((it.next(), it.next()), (Some(c), Some(':')) if c.is_ascii_alphabetic())
}

/// Decode `%XX` escapes in a URI path to a UTF-8 string (best-effort; a malformed
/// escape is passed through literally).
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hi = (bytes[i + 1] as char).to_digit(16);
            let lo = (bytes[i + 2] as char).to_digit(16);
            if let (Some(h), Some(l)) = (hi, lo) {
                out.push((h * 16 + l) as u8);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drive_path_roundtrips_through_uri() {
        let uri = path_to_file_uri("C:\\Users\\me\\a b.txt");
        assert_eq!(uri, "file:///C:/Users/me/a%20b.txt");
        let back = file_uri_to_path(&uri).expect("path");
        assert_eq!(back, PathBuf::from("C:\\Users\\me\\a b.txt"));
    }

    #[test]
    fn unc_path_keeps_double_slash() {
        // A UNC path `\\server\share` → forward slashes start with `//`.
        let uri = path_to_file_uri("\\\\server\\share\\f.bin");
        assert!(uri.starts_with("file:////server/share"), "got {uri}");
        let back = file_uri_to_path(&uri).expect("path");
        assert_eq!(back, PathBuf::from("\\\\server\\share\\f.bin"));
    }

    #[test]
    fn non_file_uri_is_skipped() {
        assert!(file_uri_to_path("https://example.com/x").is_none());
    }

    #[test]
    fn uri_list_blob_has_header_and_double_nul() {
        let list = b"file:///C:/a.txt\nfile:///C:/b.txt";
        let blob = uri_list_to_hdrop_bytes(list).expect("blob");
        let header = std::mem::size_of::<DROPFILES>();
        // Blob is header + path data; must end in two NUL bytes (last path NUL +
        // list terminator) and be longer than the bare header.
        assert!(blob.len() > header);
        assert_eq!(&blob[blob.len() - 2..], &[0, 0]);
    }

    #[test]
    fn comments_and_blanks_are_ignored() {
        let list = b"# a comment\n\nfile:///C:/only.txt\n";
        let blob = uri_list_to_hdrop_bytes(list).expect("blob");
        // Exactly one path → header + ("C:\only.txt"+NUL) + final NUL.
        let one = "C:\\only.txt";
        let path_units = one.encode_utf16().count() + 1; // + NUL
        let expected = std::mem::size_of::<DROPFILES>() + path_units * 2 + 2;
        assert_eq!(blob.len(), expected);
    }

    #[test]
    fn invalid_utf8_uri_list_errors() {
        let err = uri_list_to_hdrop_bytes(&[0xff, 0xfe]).expect_err("must error");
        assert!(err.downcast_ref::<ClipboardError>().is_some());
    }
}
