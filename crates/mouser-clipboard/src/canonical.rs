//! Canonicalization + hashing for clipboard payloads (§7.7).
//!
//! The pull/dedup/integrity key for a representation is
//! `hash = SHA-256(canonical(format, bytes))`. Both ends MUST canonicalize the same
//! way or a hash computed on one side won't verify on the other, so the rules are
//! collected here (away from the state machine) and unit-tested directly:
//!
//! | `ClipFormat` | `canonical(format, bytes)` |
//! |--------------|----------------------------|
//! | `Utf8Text`   | UTF-8 with **CRLF→LF** and **no trailing NUL** |
//! | `Html`       | bytes **as-is** |
//! | `Rtf`        | bytes **as-is** |
//! | `Png`        | the raw PNG byte stream **as-is** |
//! | `UriList`    | UTF-8, **LF-separated**, **no trailing blank line** |
//! | `Unknown`    | bytes **as-is** (forward-compat: never offered/applied locally) |
//!
//! Canonicalization is *byte-oriented* and never fails: invalid UTF-8 is left
//! untouched (we operate on bytes, not `str`) so a malformed clipboard payload still
//! hashes deterministically rather than panicking — the engine stays I/O-free and
//! total.

use mouser_protocol::ClipFormat;

use crate::Hash;

/// CRLF (`\r\n`) — normalized to a single LF in text/uri-list canonical forms.
const CRLF: &[u8] = b"\r\n";

/// Return the canonical bytes for `(format, bytes)` per §7.7. Pure and total.
#[must_use]
pub fn canonical(format: ClipFormat, bytes: &[u8]) -> Vec<u8> {
    match format {
        ClipFormat::Utf8Text => canonical_text(bytes),
        ClipFormat::UriList => canonical_uri_list(bytes),
        // html / rtf / png ride as-is; an Unknown format is never offered or applied
        // locally (the engine gates on the known set), but hashing it as-is keeps the
        // function total for any caller.
        ClipFormat::Html | ClipFormat::Rtf | ClipFormat::Png | ClipFormat::Unknown => {
            bytes.to_vec()
        }
    }
}

/// `hash = SHA-256(canonical(format, bytes))` (§7.7) — the offer/pull/dedup key.
#[must_use]
pub fn content_hash(format: ClipFormat, bytes: &[u8]) -> Hash {
    sha256(&canonical(format, bytes))
}

/// SHA-256 of a byte slice. Shared by hashing canonical content and verifying a
/// reassembled payload (where the *already-canonical* bytes are hashed directly).
#[must_use]
pub fn sha256(bytes: &[u8]) -> Hash {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(bytes);
    h.finalize().into()
}

/// `utf8_text` canonical form: CRLF→LF, then strip any trailing NUL bytes (§7.7).
/// Operates on raw bytes so non-UTF-8 input is preserved rather than rejected.
fn canonical_text(bytes: &[u8]) -> Vec<u8> {
    let mut out = crlf_to_lf(bytes);
    while out.last() == Some(&0) {
        out.pop();
    }
    out
}

/// `uri_list` canonical form: CRLF→LF, then drop a single trailing blank line so a
/// list ending in `\n` (or `\r\n`) hashes the same as one without the terminator
/// (§7.7 "LF-separated, no trailing blank line").
fn canonical_uri_list(bytes: &[u8]) -> Vec<u8> {
    let mut out = crlf_to_lf(bytes);
    if out.last() == Some(&b'\n') {
        out.pop();
    }
    out
}

/// Replace every `\r\n` with `\n`. A lone `\r` (no following `\n`) is left as-is —
/// §7.7 names only the CRLF→LF rule, so we do not invent a CR→LF normalization.
fn crlf_to_lf(bytes: &[u8]) -> Vec<u8> {
    if !contains_crlf(bytes) {
        return bytes.to_vec();
    }
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match (bytes.get(i), bytes.get(i + 1)) {
            (Some(&b'\r'), Some(&b'\n')) => {
                out.push(b'\n');
                i += 2;
            }
            (Some(&b), _) => {
                out.push(b);
                i += 1;
            }
            (None, _) => break,
        }
    }
    out
}

/// Whether `bytes` contains a CRLF pair (cheap scan to skip allocation in the common
/// already-LF case).
fn contains_crlf(bytes: &[u8]) -> bool {
    bytes.windows(2).any(|w| w == CRLF)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_normalizes_crlf_to_lf() {
        assert_eq!(canonical(ClipFormat::Utf8Text, b"a\r\nb\r\nc"), b"a\nb\nc");
        // lone CR is preserved (only CRLF is a normalization target).
        assert_eq!(canonical(ClipFormat::Utf8Text, b"a\rb"), b"a\rb");
        // already-LF is unchanged.
        assert_eq!(canonical(ClipFormat::Utf8Text, b"a\nb"), b"a\nb");
    }

    #[test]
    fn text_strips_trailing_nul() {
        assert_eq!(canonical(ClipFormat::Utf8Text, b"hi\0\0"), b"hi");
        // interior NULs are kept; only *trailing* ones are stripped.
        assert_eq!(canonical(ClipFormat::Utf8Text, b"a\0b"), b"a\0b");
    }

    #[test]
    fn text_crlf_and_trailing_nul_together() {
        assert_eq!(canonical(ClipFormat::Utf8Text, b"x\r\ny\0"), b"x\ny");
    }

    #[test]
    fn html_and_rtf_and_png_are_verbatim() {
        let raw = b"\r\n<p>\0\xFF";
        assert_eq!(canonical(ClipFormat::Html, raw), raw);
        assert_eq!(canonical(ClipFormat::Rtf, raw), raw);
        assert_eq!(canonical(ClipFormat::Png, raw), raw);
    }

    #[test]
    fn uri_list_drops_single_trailing_blank() {
        assert_eq!(
            canonical(ClipFormat::UriList, b"file:///a\r\nfile:///b\r\n"),
            b"file:///a\nfile:///b"
        );
        // exactly one trailing newline is dropped; a second blank line survives.
        assert_eq!(
            canonical(ClipFormat::UriList, b"file:///a\n\n"),
            b"file:///a\n"
        );
        // no trailing newline → unchanged.
        assert_eq!(canonical(ClipFormat::UriList, b"file:///a"), b"file:///a");
    }

    #[test]
    fn hash_is_canonical_then_sha256() {
        // CRLF and LF variants of the same text hash identically (CRLF→LF), and the
        // hash is SHA-256 of that canonical form. Trailing LF is preserved for text
        // (only uri_list drops it), so the canonical form of "line\r\n" is "line\n".
        let crlf = content_hash(ClipFormat::Utf8Text, b"line\r\n");
        let lf = content_hash(ClipFormat::Utf8Text, b"line\n");
        assert_eq!(crlf, lf);
        assert_eq!(crlf, sha256(b"line\n"));
        // and that is distinct from the no-newline form.
        assert_ne!(crlf, content_hash(ClipFormat::Utf8Text, b"line"));
    }

    #[test]
    fn non_utf8_text_is_hashed_not_rejected() {
        // invalid UTF-8 (lone 0xFF) is preserved byte-wise and still hashes.
        let h = content_hash(ClipFormat::Utf8Text, b"\xFFok");
        assert_eq!(h, sha256(b"\xFFok"));
    }
}
