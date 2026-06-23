//! Control-stream framing (§0.2): `len: u32 (LE) | type: u16 (LE) | flags: u16 (LE) | payload`,
//! where `len` counts `type + flags + payload`. An unknown `type` is skippable: read `len`,
//! consume that many bytes, continue.

/// Maximum control-stream message size (§0.3): 256 KiB covering type + flags + payload.
pub const MAX_CONTROL_FRAME: u32 = 256 * 1024;

/// Maximum **bulk-stream** message size (§0.3). Bulk frames may be larger than the
/// control cap; `FileChunk.data` ≤ 1 MiB, so a framed chunk needs a little headroom
/// above 1 MiB for the CBOR map keys + the §0.2 header. 2 MiB gives ample margin while
/// still bounding a single allocation. The §0.2 frame *format* is identical to the
/// control stream — only the size ceiling differs per plane (§6.2).
pub const MAX_BULK_FRAME: u32 = 2 * 1024 * 1024;

const HEADER: usize = 8; // len(4) + type(2) + flags(2)

/// A decoded control-stream frame borrowing its payload from the input buffer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame<'a> {
    pub msg_type: u16,
    pub flags: u16,
    pub payload: &'a [u8],
}

/// Errors that can arise while framing or deframing a control message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameError {
    /// The buffer does not yet contain a complete frame.
    Truncated,
    /// `len` exceeds [`MAX_CONTROL_FRAME`] or is otherwise invalid.
    TooLarge,
}

impl core::fmt::Display for FrameError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            FrameError::Truncated => f.write_str("frame truncated"),
            FrameError::TooLarge => f.write_str("frame length out of range"),
        }
    }
}

impl std::error::Error for FrameError {}

/// Encode a control-stream frame around an already-encoded `payload` (≤ 256 KiB).
pub fn encode_frame(msg_type: u16, flags: u16, payload: &[u8]) -> Result<Vec<u8>, FrameError> {
    encode_frame_capped(msg_type, flags, payload, MAX_CONTROL_FRAME)
}

/// Encode a **bulk-stream** frame (§6.2) around `payload`, allowing up to
/// [`MAX_BULK_FRAME`] so a 1 MiB `FileChunk` fits once CBOR-wrapped. Same §0.2 layout.
pub fn encode_bulk_frame(msg_type: u16, flags: u16, payload: &[u8]) -> Result<Vec<u8>, FrameError> {
    encode_frame_capped(msg_type, flags, payload, MAX_BULK_FRAME)
}

/// Encode a §0.2 frame around `payload`, rejecting it if `len` would exceed `max`.
pub fn encode_frame_capped(
    msg_type: u16,
    flags: u16,
    payload: &[u8],
    max: u32,
) -> Result<Vec<u8>, FrameError> {
    let len = (HEADER - 4)
        .checked_add(payload.len())
        .ok_or(FrameError::TooLarge)?;
    if len > max as usize {
        return Err(FrameError::TooLarge);
    }
    let mut out = Vec::with_capacity(4 + len);
    out.extend_from_slice(&(len as u32).to_le_bytes());
    out.extend_from_slice(&msg_type.to_le_bytes());
    out.extend_from_slice(&flags.to_le_bytes());
    out.extend_from_slice(payload);
    Ok(out)
}

/// Decode one frame from the front of `buf`, returning the frame and the number of
/// bytes it occupied (so the caller can advance and decode the next frame).
pub fn decode_frame(buf: &[u8]) -> Result<(Frame<'_>, usize), FrameError> {
    let len_bytes: [u8; 4] = buf
        .get(0..4)
        .ok_or(FrameError::Truncated)?
        .try_into()
        .map_err(|_| FrameError::Truncated)?;
    let len = u32::from_le_bytes(len_bytes);
    if len < (HEADER - 4) as u32 || len > MAX_CONTROL_FRAME {
        return Err(FrameError::TooLarge);
    }
    let total = 4usize
        .checked_add(len as usize)
        .ok_or(FrameError::TooLarge)?;
    let frame = buf.get(0..total).ok_or(FrameError::Truncated)?;
    let type_bytes: [u8; 2] = frame
        .get(4..6)
        .ok_or(FrameError::Truncated)?
        .try_into()
        .map_err(|_| FrameError::Truncated)?;
    let flag_bytes: [u8; 2] = frame
        .get(6..8)
        .ok_or(FrameError::Truncated)?
        .try_into()
        .map_err(|_| FrameError::Truncated)?;
    let payload = frame.get(HEADER..total).ok_or(FrameError::Truncated)?;
    Ok((
        Frame {
            msg_type: u16::from_le_bytes(type_bytes),
            flags: u16::from_le_bytes(flag_bytes),
            payload,
        },
        total,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrips_a_max_size_control_payload() {
        let payload = vec![0xABu8; (MAX_CONTROL_FRAME as usize) - (HEADER - 4)];
        let encoded = encode_frame(0x0102, 0x0304, &payload).expect("encode max payload");
        let (frame, total) = decode_frame(&encoded).expect("decode max payload");
        assert_eq!(frame.msg_type, 0x0102);
        assert_eq!(frame.flags, 0x0304);
        assert_eq!(frame.payload, &payload[..]);
        assert_eq!(total, encoded.len());
    }

    #[test]
    fn encode_rejects_oversized_control_payload() {
        // One byte past the cap (the cap counts type+flags+payload).
        let payload = vec![0u8; (MAX_CONTROL_FRAME as usize) - (HEADER - 4) + 1];
        assert_eq!(encode_frame(0, 0, &payload), Err(FrameError::TooLarge));
    }

    #[test]
    fn encode_bulk_allows_payload_above_control_cap() {
        // A payload larger than the control cap but within the bulk cap must encode as a
        // bulk frame and be rejected as a control frame — the two planes have distinct
        // ceilings (§0.3).
        let payload = vec![0u8; MAX_CONTROL_FRAME as usize + 1];
        assert!(encode_bulk_frame(0, 0, &payload).is_ok());
        assert_eq!(encode_frame(0, 0, &payload), Err(FrameError::TooLarge));
    }

    #[test]
    fn decode_rejects_a_huge_declared_length_without_allocating() {
        // An attacker-supplied len of 0xFFFF_FFFF must be rejected by the cap before any
        // caller is told to allocate that much — the memory-exhaustion guard.
        let mut buf = Vec::new();
        buf.extend_from_slice(&u32::MAX.to_le_bytes());
        buf.extend_from_slice(&[0u8; 4]); // type + flags
        assert_eq!(decode_frame(&buf), Err(FrameError::TooLarge));
    }

    #[test]
    fn decode_rejects_len_below_the_header_minimum() {
        // len must cover at least type(2)+flags(2) = 4. len = 3 is malformed.
        let mut buf = Vec::new();
        buf.extend_from_slice(&3u32.to_le_bytes());
        buf.extend_from_slice(&[0u8; 4]);
        assert_eq!(decode_frame(&buf), Err(FrameError::TooLarge));
    }

    #[test]
    fn decode_reports_truncation_one_byte_short() {
        let payload = [1u8, 2, 3, 4];
        let encoded = encode_frame(0x11, 0x22, &payload).expect("encode");
        let short = &encoded[..encoded.len() - 1];
        assert_eq!(decode_frame(short), Err(FrameError::Truncated));
        // The full buffer still decodes.
        assert!(decode_frame(&encoded).is_ok());
    }

    #[test]
    fn decode_reports_truncation_on_a_partial_header() {
        assert_eq!(decode_frame(&[0u8; 3]), Err(FrameError::Truncated));
    }
}
