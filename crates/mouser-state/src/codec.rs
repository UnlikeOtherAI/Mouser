use automerge::ScalarValue;

/// Render a 32-byte `device_id` as lowercase hex — the map-key form used by the
/// CRDT schema (spec Appendix A: `Map<device_id_hex, …>`).
#[must_use]
pub fn device_id_hex(id: &[u8; 32]) -> String {
    let mut s = String::with_capacity(64);
    for b in id {
        s.push(char::from_digit((b >> 4) as u32, 16).unwrap_or('0'));
        s.push(char::from_digit((b & 0x0f) as u32, 16).unwrap_or('0'));
    }
    s
}

pub(crate) fn lww_key(rev: u64, editor: &[u8; 32]) -> String {
    format!("{rev:020}|{}", device_id_hex(editor))
}

/// Encode the LWW key `(rev, editor)` as a sortable string: a zero-padded
/// 20-digit decimal revision, a separator, then the 64-char editor hex. Plain
/// lexicographic comparison then matches the `(rev, editor)` ordering exactly.
pub(crate) fn encode_lww(rev: u64, editor: &[u8; 32]) -> ScalarValue {
    ScalarValue::Str(lww_key(rev, editor).into())
}

/// Inverse of [`encode_lww`]; returns `None` on a malformed register value.
pub(crate) fn decode_lww(s: &str) -> Option<(u64, [u8; 32])> {
    let (rev_str, editor_hex) = s.split_once('|')?;
    let rev: u64 = rev_str.parse().ok()?;
    let editor = hex32(editor_hex)?;
    Some((rev, editor))
}

/// Parse exactly 64 lowercase/uppercase hex chars into a 32-byte array.
fn hex32(s: &str) -> Option<[u8; 32]> {
    if s.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    let bytes = s.as_bytes();
    for (i, slot) in out.iter_mut().enumerate() {
        let hi = hex_nibble(*bytes.get(i * 2)?)?;
        let lo = hex_nibble(*bytes.get(i * 2 + 1)?)?;
        *slot = (hi << 4) | lo;
    }
    Some(out)
}

fn hex_nibble(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}
