//! Control-stream framing (§0.2): `len: u32 (LE) | type: u16 (LE) | flags: u16 (LE) | payload`,
//! where `len` counts `type + flags + payload`. An unknown `type` is skippable: read `len`,
//! consume that many bytes, continue.

/// Maximum control-stream message size (§0.3): 256 KiB covering type + flags + payload.
pub const MAX_CONTROL_FRAME: u32 = 256 * 1024;

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

/// Encode a control-stream frame around an already-encoded `payload`.
pub fn encode_frame(msg_type: u16, flags: u16, payload: &[u8]) -> Result<Vec<u8>, FrameError> {
    let len = (HEADER - 4)
        .checked_add(payload.len())
        .ok_or(FrameError::TooLarge)?;
    if len > MAX_CONTROL_FRAME as usize {
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
