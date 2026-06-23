use crate::error::{StateError, StateResult};

/// Spec §0.3 cap for control-lane payloads, including each encoded change.
pub const CONTROL_WIRE_CAP: usize = 256 * 1024;
/// Spec §0.3 cap for a full shared-state snapshot.
pub const SNAPSHOT_WIRE_CAP: usize = 8 * 1024 * 1024;

const MAGIC_BYTES: [u8; 4] = [0x85, 0x6f, 0x4a, 0x83];
const CHUNK_DOCUMENT: u8 = 0;
const CHUNK_COMPRESSED: u8 = 2;
const COLUMN_DEFLATE: u32 = 0b0000_1000;

pub(crate) fn validate_change_bytes(bytes: &[u8]) -> StateResult<()> {
    validate_len("change", bytes.len(), CONTROL_WIRE_CAP)?;
    scan_chunks(bytes)
}

pub(crate) fn validate_snapshot_bytes(bytes: &[u8]) -> StateResult<()> {
    validate_len("snapshot", bytes.len(), SNAPSHOT_WIRE_CAP)?;
    scan_chunks(bytes)
}

fn validate_len(kind: &str, len: usize, cap: usize) -> StateResult<()> {
    if len > cap {
        Err(StateError::Decode(format!(
            "{kind} payload exceeds {cap} bytes"
        )))
    } else {
        Ok(())
    }
}

fn scan_chunks(bytes: &[u8]) -> StateResult<()> {
    let mut pos = 0;
    while pos < bytes.len() {
        let chunk_type = read_chunk_type(bytes, &mut pos)?;
        let chunk_len = read_len(bytes, &mut pos)?;
        let chunk = take(bytes, &mut pos, chunk_len)?;
        match chunk_type {
            CHUNK_COMPRESSED => {
                return Err(StateError::Decode(
                    "compressed automerge chunks are not accepted".to_owned(),
                ));
            }
            CHUNK_DOCUMENT => reject_deflated_document_columns(chunk)?,
            _ => {}
        }
    }
    Ok(())
}

fn read_chunk_type(bytes: &[u8], pos: &mut usize) -> StateResult<u8> {
    let magic = take(bytes, pos, MAGIC_BYTES.len())?;
    if magic != MAGIC_BYTES {
        return Err(StateError::Decode(
            "invalid automerge chunk magic".to_owned(),
        ));
    }
    let _checksum = take(bytes, pos, 4)?;
    read_byte(bytes, pos)
}

fn reject_deflated_document_columns(chunk: &[u8]) -> StateResult<()> {
    let mut pos = 0;
    skip_actors(chunk, &mut pos)?;
    skip_hashes(chunk, &mut pos)?;
    let changes = read_raw_columns(chunk, &mut pos)?;
    let ops = read_raw_columns(chunk, &mut pos)?;
    if changes.has_deflate || ops.has_deflate {
        Err(StateError::Decode(
            "deflated automerge document columns are not accepted".to_owned(),
        ))
    } else {
        Ok(())
    }
}

fn skip_actors(bytes: &[u8], pos: &mut usize) -> StateResult<()> {
    let count = read_len(bytes, pos)?;
    for _ in 0..count {
        let len = read_len(bytes, pos)?;
        let _actor = take(bytes, pos, len)?;
    }
    Ok(())
}

fn skip_hashes(bytes: &[u8], pos: &mut usize) -> StateResult<()> {
    let count = read_len(bytes, pos)?;
    for _ in 0..count {
        let _hash = take(bytes, pos, 32)?;
    }
    Ok(())
}

struct RawColumns {
    has_deflate: bool,
}

fn read_raw_columns(bytes: &[u8], pos: &mut usize) -> StateResult<RawColumns> {
    let count = read_len(bytes, pos)?;
    let mut has_deflate = false;
    for _ in 0..count {
        let spec = read_u32(bytes, pos)?;
        let _len = read_len(bytes, pos)?;
        has_deflate |= spec & COLUMN_DEFLATE != 0;
    }
    Ok(RawColumns { has_deflate })
}

fn read_u32(bytes: &[u8], pos: &mut usize) -> StateResult<u32> {
    let raw = read_leb_u64(bytes, pos)?;
    u32::try_from(raw).map_err(|_| StateError::Decode("u32 LEB128 overflow".to_owned()))
}

fn read_len(bytes: &[u8], pos: &mut usize) -> StateResult<usize> {
    let raw = read_leb_u64(bytes, pos)?;
    usize::try_from(raw).map_err(|_| StateError::Decode("length overflow".to_owned()))
}

fn read_leb_u64(bytes: &[u8], pos: &mut usize) -> StateResult<u64> {
    let mut out = 0u64;
    let mut shift = 0u32;
    for _ in 0..10 {
        let byte = read_byte(bytes, pos)?;
        let bits = u64::from(byte & 0x7f);
        out |= bits
            .checked_shl(shift)
            .ok_or_else(|| StateError::Decode("LEB128 overflow".to_owned()))?;
        if byte & 0x80 == 0 {
            return Ok(out);
        }
        shift += 7;
    }
    Err(StateError::Decode("LEB128 too long".to_owned()))
}

fn read_byte(bytes: &[u8], pos: &mut usize) -> StateResult<u8> {
    let b = bytes
        .get(*pos)
        .copied()
        .ok_or_else(|| StateError::Decode("truncated automerge chunk".to_owned()))?;
    *pos += 1;
    Ok(b)
}

fn take<'a>(bytes: &'a [u8], pos: &mut usize, len: usize) -> StateResult<&'a [u8]> {
    let end = pos
        .checked_add(len)
        .ok_or_else(|| StateError::Decode("chunk length overflow".to_owned()))?;
    let out = bytes
        .get(*pos..end)
        .ok_or_else(|| StateError::Decode("truncated automerge chunk".to_owned()))?;
    *pos = end;
    Ok(out)
}
