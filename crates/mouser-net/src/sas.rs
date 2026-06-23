//! The §5 mandatory **Short Authentication String (SAS)** pairing computation.
//!
//! After the interactive QUIC handshake + cert pin (§3), both peers derive an identical
//! 6-digit decimal string the user compares on-screen to authenticate the channel against
//! a relay/MITM. The derivation (§5 step 3) is:
//!
//! ```text
//! ctx    = min(idA, idB) || max(idA, idB)   // the two 32-byte device_ids, ascending by byte value
//! k      = tls_exporter(label="mouser-sas-v1", context=ctx, length=32)   // RFC 5705 / RFC 8446 §7.5
//! digest = HKDF-SHA256(salt="mouser-sas-v1", ikm=k, info="sas", L=4)
//! SAS    = be_u32(digest) mod 1_000_000      // rendered as 6 zero-padded decimal digits
//! ```
//!
//! Both ends derive the **same** string because the TLS 1.3 exporter is symmetric across
//! the connection and the ascending-id `context` is order-independent (each side sorts the
//! two ids the same way, regardless of who dialed). The exporter binds `k` to *this* TLS
//! session, so a relay that re-terminates TLS cannot reproduce it.
//!
//! This mirrors the prior art in [`crate::bulk`] (the §5 step-5 `channel_sig` over the bulk
//! exporter): one symmetric, session-bound key-derivation root, no divergent copy.

use hkdf::Hkdf;
use mouser_core::DeviceId;
use quinn::Connection;
use sha2::Sha256;

use crate::NetError;

/// TLS exporter label **and** HKDF salt for SAS derivation (§5 step 3).
const SAS_LABEL: &[u8] = b"mouser-sas-v1";
/// HKDF `info` string (§5 step 3).
const SAS_INFO: &[u8] = b"sas";
/// Exporter output length fed as HKDF `ikm` (§5 step 3).
const SAS_EXPORT_LEN: usize = 32;
/// HKDF output length `L` — four bytes consumed big-endian (§5 step 3).
const SAS_DIGEST_LEN: usize = 4;
/// Modulus reducing the 32-bit digest to six decimal digits (§5 step 3).
const SAS_MODULUS: u32 = 1_000_000;

/// Compute the 6-digit SAS for the connection between `my_id` and `peer_id` (§5 step 3).
///
/// `conn` must be the **interactive** connection (the exporter is taken over it). Returns
/// the canonical `"{:06}"` rendering. The two device ids are sorted ascending to build the
/// exporter `context`, so calling this on either end with the local/peer ids swapped yields
/// the identical string. Panic-free: every fallible step maps to [`NetError`].
pub fn compute_sas(
    conn: &Connection,
    my_id: DeviceId,
    peer_id: DeviceId,
) -> Result<String, NetError> {
    let context = exporter_context(my_id, peer_id)?;

    // k = tls_exporter(label="mouser-sas-v1", context=ctx, length=32) over the interactive
    // connection (RFC 5705 / RFC 8446 §7.5).
    let mut k = [0u8; SAS_EXPORT_LEN];
    conn.export_keying_material(&mut k, SAS_LABEL, &context)
        .map_err(|_| NetError::Tls("sas exporter unavailable".to_string()))?;

    sas_from_exporter(&k)
}

fn exporter_context(my_id: DeviceId, peer_id: DeviceId) -> Result<[u8; 64], NetError> {
    let (low, high) = if my_id <= peer_id {
        (my_id, peer_id)
    } else {
        (peer_id, my_id)
    };
    let mut context = [0u8; 64];
    context
        .get_mut(0..32)
        .ok_or_else(|| NetError::Tls("sas context build failed".to_string()))?
        .copy_from_slice(&low);
    context
        .get_mut(32..64)
        .ok_or_else(|| NetError::Tls("sas context build failed".to_string()))?
        .copy_from_slice(&high);
    Ok(context)
}

fn sas_from_exporter(k: &[u8; SAS_EXPORT_LEN]) -> Result<String, NetError> {
    // digest = HKDF-SHA256(salt="mouser-sas-v1", ikm=k, info="sas", L=4).
    let hk = Hkdf::<Sha256>::new(Some(SAS_LABEL), k);
    let mut digest = [0u8; SAS_DIGEST_LEN];
    hk.expand(SAS_INFO, &mut digest)
        .map_err(|_| NetError::Tls("sas hkdf expand failed".to_string()))?;

    // SAS = be_u32(digest) mod 1_000_000, rendered as 6 zero-padded decimal digits.
    let value = u32::from_be_bytes(digest) % SAS_MODULUS;
    Ok(format!("{value:06}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascending_context_is_order_independent() {
        let a: DeviceId = [1u8; 32];
        let b: DeviceId = [2u8; 32];
        assert_eq!(
            exporter_context(a, b).expect("context ab"),
            exporter_context(b, a).expect("context ba"),
            "min||max must be order-independent"
        );
    }

    #[test]
    fn ascending_context_changes_for_different_device_pairs() {
        let a: DeviceId = [1u8; 32];
        let b: DeviceId = [2u8; 32];
        let c: DeviceId = [3u8; 32];
        assert_ne!(
            exporter_context(a, b).expect("context ab"),
            exporter_context(a, c).expect("context ac"),
            "changing the ordered device-id pair must change the exporter context"
        );
    }

    #[test]
    fn sas_rendering_is_deterministic_for_exporter_input() {
        let first = [0xA5u8; SAS_EXPORT_LEN];
        let second = [0x5Au8; SAS_EXPORT_LEN];
        let a = sas_from_exporter(&first).expect("first sas");
        assert_eq!(
            sas_from_exporter(&first).expect("first sas again"),
            a,
            "same exporter input must produce the same SAS"
        );
        assert_ne!(
            sas_from_exporter(&second).expect("second sas"),
            a,
            "different exporter input should produce a different SAS"
        );
    }

    #[test]
    fn rendering_is_six_zero_padded_digits() {
        // Reduction + formatting maps any u32 to exactly six decimal digits.
        for value in [0u32, 7, 999_999, 1_234_567] {
            let reduced = value % SAS_MODULUS;
            let s = format!("{reduced:06}");
            assert_eq!(s.len(), 6, "SAS must be 6 digits, got {s:?}");
            assert!(
                s.chars().all(|c| c.is_ascii_digit()),
                "SAS must be all digits, got {s:?}"
            );
        }
    }
}
