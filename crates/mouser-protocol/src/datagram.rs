//! Pointer-motion datagram plane (§7.6). Datagrams are encoded with **postcard**
//! (fixed layout, little-endian scalars) behind a **1-byte tag** that selects the
//! motion mode so a receiver can always discriminate:
//!
//! - tag `0x01` → [`PointerMotion`] (absolute, default).
//! - tag `0x02` → [`PointerMotionRel`] (relative / pointer-locked, cumulative deltas).
//!
//! An unknown tag decodes to [`Datagram::Unknown`] so the receiver can drop it
//! without erroring (forward-compatibility, §7.6 "unknown tag → drop").

use serde::{Deserialize, Serialize};

/// Datagram tag for absolute [`PointerMotion`].
pub const TAG_POINTER_MOTION: u8 = 0x01;
/// Datagram tag for relative [`PointerMotionRel`].
pub const TAG_POINTER_MOTION_REL: u8 = 0x02;

/// `PointerMotion` (tag 0x01, absolute) — integer logical-pixel position in the
/// target display's space (§7.6). Loss self-heals because positions are absolute.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PointerMotion {
    /// Ownership epoch under which this sample is valid (§7.4); a change resets `seq`.
    pub owner_epoch: u64,
    /// Wraparound-safe sequence (RFC 1982); newest-wins.
    pub seq: u32,
    /// Target display id (Appendix A).
    pub display_id: u32,
    /// Logical-pixel x, origin top-left, receiver clamps.
    pub x: i32,
    /// Logical-pixel y, y-down, receiver clamps.
    pub y: i32,
}

/// `PointerMotionRel` (tag 0x02, relative) — cumulative deltas since session start
/// for pointer-locked / relative consumers (§7.6). Still newest-wins, not per-packet.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PointerMotionRel {
    /// Ownership epoch under which this sample is valid (§7.4); a change resets `seq`.
    pub owner_epoch: u64,
    /// Wraparound-safe sequence (RFC 1982); newest-wins.
    pub seq: u32,
    /// Cumulative x delta since session start.
    pub dx_acc: i64,
    /// Cumulative y delta since session start.
    pub dy_acc: i64,
}

/// A decoded motion datagram, or [`Datagram::Unknown`] for an unrecognized tag.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Datagram {
    /// Absolute pointer motion (tag 0x01).
    Motion(PointerMotion),
    /// Relative pointer motion (tag 0x02).
    MotionRel(PointerMotionRel),
    /// Unrecognized tag — receivers MUST drop (§7.6).
    Unknown(u8),
}

/// A datagram encode/decode failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DatagramError {
    /// Empty buffer — no tag byte present.
    Empty,
    /// postcard (de)serialization failed.
    Codec(String),
}

impl core::fmt::Display for DatagramError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            DatagramError::Empty => f.write_str("empty datagram"),
            DatagramError::Codec(m) => write!(f, "datagram codec: {m}"),
        }
    }
}

impl std::error::Error for DatagramError {}

/// Encode an absolute [`PointerMotion`] datagram (tag 0x01 + postcard body).
pub fn encode_motion(motion: &PointerMotion) -> Result<Vec<u8>, DatagramError> {
    encode_tagged(TAG_POINTER_MOTION, motion)
}

/// Encode a relative [`PointerMotionRel`] datagram (tag 0x02 + postcard body).
pub fn encode_motion_rel(motion: &PointerMotionRel) -> Result<Vec<u8>, DatagramError> {
    encode_tagged(TAG_POINTER_MOTION_REL, motion)
}

fn encode_tagged<T: Serialize>(tag: u8, body: &T) -> Result<Vec<u8>, DatagramError> {
    let mut out = vec![tag];
    let encoded = postcard::to_allocvec(body).map_err(|e| DatagramError::Codec(e.to_string()))?;
    out.extend_from_slice(&encoded);
    Ok(out)
}

/// Decode a motion datagram from `buf`, dispatching on the 1-byte tag. An unknown
/// tag yields [`Datagram::Unknown`] (drop, never error) per §7.6.
pub fn decode_datagram(buf: &[u8]) -> Result<Datagram, DatagramError> {
    let (&tag, body) = buf.split_first().ok_or(DatagramError::Empty)?;
    match tag {
        TAG_POINTER_MOTION => {
            let m = postcard::from_bytes(body).map_err(|e| DatagramError::Codec(e.to_string()))?;
            Ok(Datagram::Motion(m))
        }
        TAG_POINTER_MOTION_REL => {
            let m = postcard::from_bytes(body).map_err(|e| DatagramError::Codec(e.to_string()))?;
            Ok(Datagram::MotionRel(m))
        }
        other => Ok(Datagram::Unknown(other)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absolute_motion_roundtrips_with_tag() {
        let m = PointerMotion {
            owner_epoch: 3,
            seq: 42,
            display_id: 1,
            x: -10,
            y: 200,
        };
        let bytes = encode_motion(&m).expect("encode");
        assert_eq!(bytes.first(), Some(&TAG_POINTER_MOTION));
        assert_eq!(
            decode_datagram(&bytes).expect("decode"),
            Datagram::Motion(m)
        );
    }

    #[test]
    fn relative_motion_roundtrips_with_tag() {
        let m = PointerMotionRel {
            owner_epoch: 7,
            seq: 1,
            dx_acc: -5,
            dy_acc: 9000,
        };
        let bytes = encode_motion_rel(&m).expect("encode");
        assert_eq!(bytes.first(), Some(&TAG_POINTER_MOTION_REL));
        assert_eq!(
            decode_datagram(&bytes).expect("decode"),
            Datagram::MotionRel(m)
        );
    }

    #[test]
    fn unknown_tag_is_dropped_not_errored() {
        assert_eq!(
            decode_datagram(&[0xFF, 1, 2]).expect("decode"),
            Datagram::Unknown(0xFF)
        );
    }

    #[test]
    fn empty_datagram_errors() {
        assert_eq!(decode_datagram(&[]), Err(DatagramError::Empty));
    }
}
