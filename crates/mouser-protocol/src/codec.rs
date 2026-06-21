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

/// Decode a CBOR control-plane payload.
pub fn from_cbor<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, CodecError> {
    ciborium::from_reader(bytes).map_err(|e| CodecError::Decode(e.to_string()))
}
