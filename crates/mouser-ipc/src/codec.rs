//! Length-prefixed CBOR framing for the IPC stream.
//!
//! Every message on the Unix-domain socket is `len: u32 (LE) | CBOR payload`, mirroring
//! the `mouser-protocol` control-stream style (a little-endian length prefix counting
//! the payload bytes). Frames are read/written with the async helpers below so neither
//! side needs to manage a parse buffer.

use std::io;

use serde::de::DeserializeOwned;
use serde::Serialize;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// Maximum IPC frame payload (256 KiB). A snapshot of LAN peers + connection state is
/// tiny; this bounds a single allocation against a malformed/oversized length prefix.
pub const MAX_FRAME: u32 = 256 * 1024;

/// Errors framing or transporting an IPC message.
#[derive(Debug, thiserror::Error)]
pub enum IpcError {
    /// Underlying socket I/O failed.
    #[error("ipc io: {0}")]
    Io(#[from] std::io::Error),
    /// A frame's length prefix exceeded [`MAX_FRAME`].
    #[error("ipc frame too large: {0} bytes")]
    TooLarge(u32),
    /// CBOR serialization of an outgoing message failed.
    #[error("ipc encode: {0}")]
    Encode(String),
    /// CBOR deserialization of an incoming message failed.
    #[error("ipc decode: {0}")]
    Decode(String),
    /// The peer closed the connection at a frame boundary (clean EOF).
    #[error("ipc connection closed")]
    Closed,
    /// The peer closed the connection after sending part of a frame.
    #[error("ipc truncated frame")]
    TruncatedFrame,
}

/// Encode `value` as CBOR and write it as one length-prefixed frame.
pub async fn write_message<W, T>(writer: &mut W, value: &T) -> Result<(), IpcError>
where
    W: AsyncWrite + Unpin,
    T: Serialize,
{
    let mut payload = Vec::new();
    ciborium::into_writer(value, &mut payload).map_err(|e| IpcError::Encode(e.to_string()))?;
    let len = u32::try_from(payload.len()).map_err(|_| IpcError::TooLarge(u32::MAX))?;
    if len > MAX_FRAME {
        return Err(IpcError::TooLarge(len));
    }
    writer.write_all(&len.to_le_bytes()).await?;
    writer.write_all(&payload).await?;
    writer.flush().await?;
    Ok(())
}

/// Read one length-prefixed frame and decode it as CBOR.
///
/// Returns [`IpcError::Closed`] if the stream ends cleanly before a frame starts (the
/// normal way to detect a disconnected peer).
pub async fn read_message<R, T>(reader: &mut R) -> Result<T, IpcError>
where
    R: AsyncRead + Unpin,
    T: DeserializeOwned,
{
    let mut len_bytes = [0u8; 4];
    if !read_exact_or_boundary(reader, &mut len_bytes).await? {
        return Err(IpcError::Closed);
    }
    let len = u32::from_le_bytes(len_bytes);
    if len > MAX_FRAME {
        return Err(IpcError::TooLarge(len));
    }
    let payload_len = usize::try_from(len).map_err(|_| IpcError::TooLarge(len))?;
    let mut payload = vec![0u8; payload_len];
    if !read_exact_or_boundary(reader, &mut payload).await? {
        return Err(IpcError::TruncatedFrame);
    }
    ciborium::from_reader(payload.as_slice()).map_err(|e| IpcError::Decode(e.to_string()))
}

async fn read_exact_or_boundary<R>(reader: &mut R, buf: &mut [u8]) -> Result<bool, IpcError>
where
    R: AsyncRead + Unpin,
{
    let mut filled = 0;
    while filled < buf.len() {
        let Some(dst) = buf.get_mut(filled..) else {
            return Err(IpcError::Io(io::Error::new(
                io::ErrorKind::InvalidData,
                "ipc frame buffer cursor exceeded buffer length",
            )));
        };
        let read = reader.read(dst).await?;
        if read == 0 {
            if filled == 0 {
                return Ok(false);
            }
            return Err(IpcError::TruncatedFrame);
        }
        filled += read;
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    #[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
    struct Probe {
        id: u64,
        name: String,
    }

    #[tokio::test]
    async fn frame_round_trips_in_memory() {
        let original = Probe {
            id: 42,
            name: "peer".to_string(),
        };
        let mut buf = Vec::new();
        write_message(&mut buf, &original).await.expect("write");
        let mut reader = buf.as_slice();
        let back: Probe = read_message(&mut reader).await.expect("read");
        assert_eq!(back, original);
    }

    #[tokio::test]
    async fn clean_eof_reports_closed() {
        let empty: &[u8] = &[];
        let mut reader = empty;
        let err = read_message::<_, Probe>(&mut reader).await.unwrap_err();
        assert!(matches!(err, IpcError::Closed));
    }

    #[tokio::test]
    async fn oversized_length_prefix_is_rejected() {
        let mut bytes = (MAX_FRAME + 1).to_le_bytes().to_vec();
        bytes.push(0);
        let mut reader = bytes.as_slice();
        let err = read_message::<_, Probe>(&mut reader).await.unwrap_err();
        assert!(matches!(err, IpcError::TooLarge(_)));
    }

    #[tokio::test]
    async fn partial_header_reports_truncated_frame() {
        let partial_header = [1u8, 0, 0];
        let mut reader = partial_header.as_slice();
        let err = read_message::<_, Probe>(&mut reader).await.unwrap_err();
        assert!(matches!(err, IpcError::TruncatedFrame));
    }

    #[tokio::test]
    async fn partial_payload_reports_truncated_frame() {
        let mut bytes = 4u32.to_le_bytes().to_vec();
        bytes.push(0);
        let mut reader = bytes.as_slice();
        let err = read_message::<_, Probe>(&mut reader).await.unwrap_err();
        assert!(matches!(err, IpcError::TruncatedFrame));
    }
}
