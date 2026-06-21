//! CBOR (de)serialization for control-plane payloads (§0.1). Errors are surfaced as
//! a small owned [`CodecError`] so the decode path stays panic-free.

use serde::{de::DeserializeOwned, Serialize};

/// A CBOR encode/decode failure for a control-plane payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CodecError {
    /// Serialization to CBOR failed.
    Encode(String),
    /// Deserialization from CBOR failed (malformed/oversized/incompatible bytes).
    Decode(String),
}

impl core::fmt::Display for CodecError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            CodecError::Encode(m) => write!(f, "cbor encode: {m}"),
            CodecError::Decode(m) => write!(f, "cbor decode: {m}"),
        }
    }
}

impl std::error::Error for CodecError {}

/// Encode a value as a CBOR control-plane payload.
pub fn to_cbor<T: Serialize>(value: &T) -> Result<Vec<u8>, CodecError> {
    let mut buf = Vec::new();
    ciborium::into_writer(value, &mut buf).map_err(|e| CodecError::Encode(e.to_string()))?;
    Ok(buf)
}

/// CBOR initial byte for an indefinite-length map (major type 5, count 31).
const INDEFINITE_MAP: u8 = 0xBF;
/// CBOR initial byte for an indefinite-length array (major type 4, count 31).
const INDEFINITE_ARRAY: u8 = 0x9F;

/// Decode a CBOR control-plane payload.
///
/// Strict decode (A6): exactly one CBOR item must occupy the whole slice. Trailing
/// bytes after the first item are rejected with `Decode("trailing bytes")` so that
/// two distinct frames can never decode to the same message (the byte-exact / golden-
/// vector interop oracle, §0.1).
///
/// Cheap M10 guard: the spec mandates definite-length encoding (§0.1). Control-plane
/// messages are top-level CBOR maps, so an outermost indefinite-length map/array head
/// is always non-conformant and is rejected here. This shallow check has no false
/// positives; *nested* indefinite-length containers are out of scope (a full
/// structural walk would be required and is not cheap).
pub fn from_cbor<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, CodecError> {
    if matches!(bytes.first(), Some(&INDEFINITE_MAP) | Some(&INDEFINITE_ARRAY)) {
        return Err(CodecError::Decode("indefinite-length top-level item".to_string()));
    }
    let mut cursor = std::io::Cursor::new(bytes);
    let value = ciborium::from_reader(&mut cursor).map_err(|e| CodecError::Decode(e.to_string()))?;
    // `Cursor::position` is the count of bytes consumed by the first item. Any bytes
    // remaining in the slice are trailing garbage and must be rejected.
    if (cursor.position() as usize) != bytes.len() {
        return Err(CodecError::Decode("trailing bytes".to_string()));
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    #[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
    struct Probe {
        id: u64,
    }

    #[test]
    fn roundtrips_exact_bytes() {
        let bytes = to_cbor(&Probe { id: 7 }).expect("encode");
        let back: Probe = from_cbor(&bytes).expect("decode");
        assert_eq!(back, Probe { id: 7 });
    }

    #[test]
    fn rejects_trailing_bytes() {
        // A valid `Probe` CBOR map followed by a stray byte must be rejected, not
        // silently truncated — otherwise two frames decode to the same message (A6).
        let mut bytes = to_cbor(&Probe { id: 7 }).expect("encode");
        bytes.push(0x00);
        assert_eq!(
            from_cbor::<Probe>(&bytes),
            Err(CodecError::Decode("trailing bytes".to_string()))
        );
    }

    #[test]
    fn rejects_indefinite_length_top_level_map() {
        // Indefinite-length map {_ "id": 7} = BF 62 69 64 07 FF. The spec mandates
        // definite-length encoding (§0.1, M10); reject it rather than accept it.
        assert_eq!(
            from_cbor::<Probe>(&[0xBF, 0x62, 0x69, 0x64, 0x07, 0xFF]),
            Err(CodecError::Decode(
                "indefinite-length top-level item".to_string()
            ))
        );
        // The definite-length equivalent A1 62 69 64 07 still decodes.
        assert_eq!(
            from_cbor::<Probe>(&[0xA1, 0x62, 0x69, 0x64, 0x07]),
            Ok(Probe { id: 7 })
        );
    }

    #[test]
    fn rejects_trailing_second_item() {
        // CBOR uint 1 (0x01) followed by a whole second item (uint 2) is two items,
        // not one — strict decode rejects the remainder.
        assert_eq!(
            from_cbor::<u8>(&[0x01, 0x02]),
            Err(CodecError::Decode("trailing bytes".to_string()))
        );
        // The single item alone still decodes.
        assert_eq!(from_cbor::<u8>(&[0x01]), Ok(1u8));
    }
}
