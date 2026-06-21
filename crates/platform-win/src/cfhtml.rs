//! CF_HTML `<->` raw-HTML-fragment bridge for the Windows clipboard adapter.
//!
//! Windows' registered `"HTML Format"` (CF_HTML) is **not** a bare HTML fragment:
//! it is the fragment wrapped in a small ASCII *description header* that records
//! byte offsets into the payload (`MS-HTML` clipboard format):
//!
//! ```text
//! Version:0.9
//! StartHTML:<10-digit byte offset>
//! EndHTML:<10-digit byte offset>
//! StartFragment:<10-digit byte offset>
//! EndFragment:<10-digit byte offset>
//! <html><body><!--StartFragment-->FRAGMENT<!--EndFragment--></body></html>
//! ```
//!
//! The wire / mac / linux clipboards all carry the **raw fragment** (`text/html`
//! bytes as-is, spec §7.7). So on Windows we must [`encode`] the raw fragment into
//! a correct CF_HTML payload on write, and [`decode`] back to the raw fragment on
//! read, so a Windows<->mac/linux round-trip preserves the HTML.
//!
//! These are pure byte functions (no Win32), so they are unit-tested on every host.

// The codec is consumed by the clipboard adapter, which is `cfg(windows)`-only.
// Off Windows it is reached solely from this module's own tests, so the non-test
// build there legitimately sees it as unused — the gate's requirement is that the
// *tests* run on every host, which they do. Allow dead code off-target only.
#![cfg_attr(not(target_os = "windows"), allow(dead_code))]

/// The marker that precedes the copied fragment inside the CF_HTML body.
const START_FRAGMENT: &str = "<!--StartFragment-->";
/// The marker that follows the copied fragment inside the CF_HTML body.
const END_FRAGMENT: &str = "<!--EndFragment-->";
/// HTML that opens the document context wrapping the fragment.
const DOC_PREFIX: &str = "<html><body>";
/// HTML that closes the document context wrapping the fragment.
const DOC_SUFFIX: &str = "</body></html>";

/// Width of each `StartHTML:`/`EndHTML:`/`StartFragment:`/`EndFragment:` offset.
///
/// Offsets are zero-padded to a **fixed** width so the header's own length does
/// not depend on the offset magnitudes — otherwise computing the offsets would be
/// self-referential. 10 digits covers any realistic clipboard payload.
const OFFSET_WIDTH: usize = 10;

/// Wrap a raw HTML fragment into a correct CF_HTML payload.
///
/// The returned bytes are exactly what belongs under the registered `"HTML
/// Format"`: an ASCII description header with byte offsets followed by the
/// fragment wrapped in `<html><body>` and the Start/End fragment markers. The
/// `StartHTML`/`EndHTML` offsets bound the whole HTML document; `StartFragment`/
/// `EndFragment` bound the original fragment bytes (between the markers).
#[must_use]
pub(crate) fn encode(fragment: &[u8]) -> Vec<u8> {
    // The header is built with placeholder offsets first so we can measure its
    // length; every offset field is the same fixed width, so the header length is
    // independent of the actual offset values and can be computed up front.
    let header_len = header_with_offsets(0, 0, 0, 0).len();

    let start_html = header_len;
    let start_fragment = header_len + DOC_PREFIX.len() + START_FRAGMENT.len();
    let end_fragment = start_fragment + fragment.len();
    let end_html = end_fragment + END_FRAGMENT.len() + DOC_SUFFIX.len();

    let header = header_with_offsets(start_html, end_html, start_fragment, end_fragment);

    let mut out = Vec::with_capacity(end_html);
    out.extend_from_slice(header.as_bytes());
    out.extend_from_slice(DOC_PREFIX.as_bytes());
    out.extend_from_slice(START_FRAGMENT.as_bytes());
    out.extend_from_slice(fragment);
    out.extend_from_slice(END_FRAGMENT.as_bytes());
    out.extend_from_slice(DOC_SUFFIX.as_bytes());
    debug_assert_eq!(out.len(), end_html);
    out
}

/// Build the CF_HTML description header with the four given byte offsets.
fn header_with_offsets(
    start_html: usize,
    end_html: usize,
    start_fragment: usize,
    end_fragment: usize,
) -> String {
    format!(
        "Version:0.9\r\n\
         StartHTML:{start_html:0OFFSET_WIDTH$}\r\n\
         EndHTML:{end_html:0OFFSET_WIDTH$}\r\n\
         StartFragment:{start_fragment:0OFFSET_WIDTH$}\r\n\
         EndFragment:{end_fragment:0OFFSET_WIDTH$}\r\n",
    )
}

/// Extract the raw HTML fragment from a CF_HTML payload.
///
/// Reads the `StartFragment`/`EndFragment` byte offsets from the description
/// header and returns the bytes between them (the original fragment). Falls back
/// to the [`START_FRAGMENT`]/[`END_FRAGMENT`] comment markers, and finally to the
/// whole input, so a malformed or already-raw payload still yields *something*
/// usable rather than an error (interop safety with non-conforming producers).
#[must_use]
pub(crate) fn decode(payload: &[u8]) -> Vec<u8> {
    if let Some(range) = fragment_range_from_header(payload) {
        return payload[range].to_vec();
    }
    if let Some(frag) = fragment_between_markers(payload) {
        return frag;
    }
    payload.to_vec()
}

/// Parse `StartFragment`/`EndFragment` from the header and return the byte range
/// they bound, if both are present and in-bounds.
fn fragment_range_from_header(payload: &[u8]) -> Option<std::ops::Range<usize>> {
    let header_end = find(payload, b"<")?.min(payload.len());
    let header = std::str::from_utf8(&payload[..header_end]).ok()?;
    let start = header_offset(header, "StartFragment:")?;
    let end = header_offset(header, "EndFragment:")?;
    if start <= end && end <= payload.len() {
        Some(start..end)
    } else {
        None
    }
}

/// Read a single `Name:NNNNNNNNNN` decimal offset out of the ASCII header.
fn header_offset(header: &str, key: &str) -> Option<usize> {
    let after = &header[header.find(key)? + key.len()..];
    let digits: String = after.chars().take_while(char::is_ascii_digit).collect();
    digits.parse().ok()
}

/// Fallback: return the bytes between the Start/End fragment comment markers.
fn fragment_between_markers(payload: &[u8]) -> Option<Vec<u8>> {
    let start = find(payload, START_FRAGMENT.as_bytes())? + START_FRAGMENT.len();
    let end = find(payload, END_FRAGMENT.as_bytes())?;
    (start <= end).then(|| payload[start..end].to_vec())
}

/// First index of `needle` in `haystack` (tiny substring search; payloads are small).
fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_a_fragment() {
        let frag = b"<b>hi</b> &amp; bye";
        let encoded = encode(frag);
        assert_eq!(decode(&encoded), frag);
    }

    #[test]
    fn round_trips_empty_fragment() {
        let encoded = encode(b"");
        assert_eq!(decode(&encoded), b"");
    }

    #[test]
    fn round_trips_unicode_fragment() {
        let frag = "<p>héllo — ✓ 𝄞</p>".as_bytes();
        let encoded = encode(frag);
        assert_eq!(decode(&encoded), frag);
    }

    #[test]
    fn header_offsets_point_at_the_real_fragment() {
        let frag = b"<i>X</i>";
        let encoded = encode(frag);
        let header_end = find(&encoded, b"<").unwrap();
        let header = std::str::from_utf8(&encoded[..header_end]).unwrap();
        let start = header_offset(header, "StartFragment:").unwrap();
        let end = header_offset(header, "EndFragment:").unwrap();
        // The bytes the header points to are exactly the original fragment.
        assert_eq!(&encoded[start..end], frag);
        // And the markers actually surround that range.
        assert_eq!(
            &encoded[start - START_FRAGMENT.len()..start],
            START_FRAGMENT.as_bytes()
        );
        assert_eq!(
            &encoded[end..end + END_FRAGMENT.len()],
            END_FRAGMENT.as_bytes()
        );
    }

    #[test]
    fn header_uses_fixed_width_offsets() {
        // Header length is identical regardless of fragment size, so the offset
        // computation in `encode` is not self-referential.
        let small = find(&encode(b"a"), b"<html").unwrap();
        let large = find(&encode(&vec![b'a'; 5000]), b"<html").unwrap();
        assert_eq!(small, large);
    }

    #[test]
    fn decode_falls_back_to_markers_without_header() {
        // No description header, just the wrapped body — decode via markers.
        let body = format!("{DOC_PREFIX}{START_FRAGMENT}<u>z</u>{END_FRAGMENT}{DOC_SUFFIX}");
        assert_eq!(decode(body.as_bytes()), b"<u>z</u>");
    }

    #[test]
    fn decode_falls_back_to_raw_for_plain_fragment() {
        // A producer that wrote a bare fragment (e.g. an old/foreign app): decode
        // returns it unchanged rather than erroring.
        let raw = b"<span>raw</span>";
        assert_eq!(decode(raw), raw);
    }
}
