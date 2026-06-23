//! Pure `CF_UNICODETEXT` text conversion helpers.
//!
//! Windows stores `CF_UNICODETEXT` as UTF-16LE text with CRLF line endings and a
//! trailing NUL. Mouser's engine-level `Utf8Text` representation is UTF-8 with LF
//! line endings, so the adapter expands LF on write and collapses CRLF on read.

// The real clipboard adapter is Windows-only. Off Windows these helpers are used
// by their host-runnable unit tests, not by the non-test build.
#![cfg_attr(not(target_os = "windows"), allow(dead_code))]

/// Decode `CF_UNICODETEXT` bytes to the engine's UTF-8/LF form.
#[must_use]
pub(crate) fn cf_unicodetext_bytes_to_engine_utf8(raw: &[u8]) -> Vec<u8> {
    let units: Vec<u16> = raw
        .chunks_exact(2)
        .filter_map(|c| match c {
            [lo, hi] => Some(u16::from_le_bytes([*lo, *hi])),
            _ => None,
        })
        .take_while(|&u| u != 0)
        .collect();
    let text = String::from_utf16_lossy(&units);
    crlf_to_lf(&text).into_bytes()
}

/// Encode engine UTF-8/LF text as `CF_UNICODETEXT` UTF-16LE bytes.
#[must_use]
pub(crate) fn engine_utf8_to_cf_unicodetext_bytes(s: &str) -> Vec<u8> {
    let text = lf_to_crlf(s);
    let mut out = Vec::with_capacity(text.len() * 2 + 2);
    for u in text.encode_utf16() {
        out.extend_from_slice(&u.to_le_bytes());
    }
    out.extend_from_slice(&0u16.to_le_bytes());
    out
}

/// Expand lone LF to CRLF, preserving existing CRLF pairs.
fn lf_to_crlf(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut previous_was_cr = false;
    for ch in s.chars() {
        if ch == '\n' && !previous_was_cr {
            out.push('\r');
        }
        out.push(ch);
        previous_was_cr = ch == '\r';
    }
    out
}

/// Collapse CRLF pairs to LF, leaving lone CR unchanged.
fn crlf_to_lf(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\r' && chars.peek() == Some(&'\n') {
            continue;
        }
        out.push(ch);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lf_to_crlf_expands_only_lone_lf() {
        assert_eq!(lf_to_crlf("a\nb\r\nc\rx\n"), "a\r\nb\r\nc\rx\r\n");
    }

    #[test]
    fn crlf_to_lf_collapses_only_pairs() {
        assert_eq!(crlf_to_lf("a\r\nb\rc\n"), "a\nb\rc\n");
    }

    #[test]
    fn cf_unicodetext_roundtrip_preserves_engine_lf() {
        let s = "hello - check\nline2\r\nline3";
        let wide = engine_utf8_to_cf_unicodetext_bytes(s);
        assert_eq!(&wide[wide.len() - 2..], &[0, 0]);
        let back = cf_unicodetext_bytes_to_engine_utf8(&wide);
        assert_eq!(back, b"hello - check\nline2\nline3");
    }

    #[test]
    fn decode_handles_unterminated_and_odd_trailing_byte() {
        let wide: Vec<u8> = "abc".encode_utf16().flat_map(u16::to_le_bytes).collect();
        assert_eq!(cf_unicodetext_bytes_to_engine_utf8(&wide), b"abc");

        let bytes = [0x41, 0x00, 0x42];
        assert_eq!(cf_unicodetext_bytes_to_engine_utf8(&bytes), b"A");
    }
}
