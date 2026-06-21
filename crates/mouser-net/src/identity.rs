//! TLS-certificate plumbing for the device identity (§3).
//!
//! The **identity itself** lives in [`mouser_core::DeviceIdentity`]: the permanent
//! Ed25519 keypair and the single `device_id = SHA-256(SubjectPublicKeyInfo)`
//! derivation (`mouser_core::device_id_from_public_key_bytes`). This module owns only
//! the two transport-specific pieces:
//!
//! 1. [`build_tls_certificate`] — build a self-signed TLS leaf cert whose public key
//!    *is* the identity key, via `rcgen`, so `SHA-256(cert SPKI) == device_id` holds
//!    by construction.
//! 2. [`device_id_from_cert`] — extract the raw Ed25519 public key from a presented
//!    leaf cert's DER `SubjectPublicKeyInfo` and feed it through the **core**
//!    derivation, so there is exactly one SPKI→hash path shared with `mouser-core`.

use ed25519_dalek::pkcs8::EncodePrivateKey;
use ed25519_dalek::SigningKey;
use mouser_core::{device_id_from_public_key_bytes, DeviceId, DeviceIdentity};
use rcgen::{CertificateParams, DistinguishedName, DnType, KeyPair, PKCS_ED25519};
use rustls_pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};

use crate::NetError;

/// A self-signed TLS leaf certificate + its private key, both DER-encoded.
pub struct TlsCertificate {
    /// The DER leaf certificate (its SPKI is the identity key, §3).
    pub cert: CertificateDer<'static>,
    /// The PKCS#8 DER private key.
    pub key: PrivateKeyDer<'static>,
}

/// Build a self-signed TLS leaf cert whose public key is `identity`'s identity key,
/// returning the DER cert chain and PKCS#8 private key for rustls (§3). Because the
/// cert carries the identity key verbatim, `device_id_from_cert(&cert)` equals
/// `identity.device_id()`.
pub fn build_tls_certificate(identity: &DeviceIdentity) -> Result<TlsCertificate, NetError> {
    // Reconstruct the signing key from the persisted seed so rcgen can sign the leaf;
    // the secret never leaves this function.
    let signing = SigningKey::from_bytes(&identity.secret_seed());
    let pkcs8 = signing
        .to_pkcs8_der()
        .map_err(|e| NetError::Identity(e.to_string()))?;
    let key_der = PrivatePkcs8KeyDer::from(pkcs8.as_bytes().to_vec());
    let key_pair = KeyPair::from_pkcs8_der_and_sign_algo(&key_der, &PKCS_ED25519)
        .map_err(|e| NetError::Identity(e.to_string()))?;

    let mut params = CertificateParams::new(vec!["mouser".to_string()])
        .map_err(|e| NetError::Identity(e.to_string()))?;
    let mut dn = DistinguishedName::new();
    dn.push(DnType::CommonName, "mouser");
    params.distinguished_name = dn;
    let cert = params
        .self_signed(&key_pair)
        .map_err(|e| NetError::Identity(e.to_string()))?;

    Ok(TlsCertificate {
        cert: cert.der().clone(),
        key: PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(pkcs8.as_bytes().to_vec())),
    })
}

/// Compute `device_id` from a presented leaf certificate (§3) — the form a cert
/// verifier uses to check `SHA-256(presented_cert_SPKI) == pinned device_id`.
///
/// The raw Ed25519 public key is extracted from the cert's DER `SubjectPublicKeyInfo`
/// and handed to [`mouser_core::device_id_from_public_key_bytes`], which validates the
/// key and performs the **single** SPKI→hash derivation shared with `mouser-core`. An
/// off-curve / malformed key is rejected here rather than yielding a bogus `device_id`.
pub fn device_id_from_cert(cert: &CertificateDer<'_>) -> Result<DeviceId, NetError> {
    let raw_key = ed25519_public_key_from_cert(cert.as_ref())?;
    device_id_from_public_key_bytes(&raw_key).map_err(|e| NetError::Identity(e.to_string()))
}

/// Extract the raw 32-byte Ed25519 public key from a DER certificate's
/// `SubjectPublicKeyInfo`. Walks `Certificate → TBSCertificate → subjectPublicKeyInfo`
/// then `SPKI → AlgorithmIdentifier, BIT STRING`, returning the BIT STRING's key bytes.
/// Panic-free (checked TLV walk); rejects anything that is not a 32-byte Ed25519 key.
fn ed25519_public_key_from_cert(der: &[u8]) -> Result<[u8; 32], NetError> {
    let mut cert = DerCursor::new(der);
    cert.enter_sequence()?; // Certificate
    cert.enter_sequence()?; // TBSCertificate
    cert.skip_optional_context0()?; // [0] version (optional)
    cert.skip_field()?; // serialNumber
    cert.skip_field()?; // signature AlgorithmIdentifier
    cert.skip_field()?; // issuer
    cert.skip_field()?; // validity
    cert.skip_field()?; // subject

    let mut spki = cert.enter_field_sequence()?; // subjectPublicKeyInfo SEQUENCE
    spki.skip_field()?; // AlgorithmIdentifier
    let bit_string = spki.read_field_content()?; // BIT STRING

    // A DER BIT STRING content is `<unused-bits><payload>`; for Ed25519 the unused-bits
    // octet is 0 and the payload is the 32-byte raw public key.
    let (&unused, key) = bit_string.split_first().ok_or_else(bad_der)?;
    if unused != 0 || key.len() != 32 {
        return Err(bad_der());
    }
    let mut out = [0u8; 32];
    out.copy_from_slice(key);
    Ok(out)
}

/// Minimal DER reader (length-prefixed TLV walk) for SPKI extraction. Panic-free.
struct DerCursor<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> DerCursor<'a> {
    fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }

    /// Descend into the SEQUENCE at the cursor, retargeting the cursor at its contents.
    fn enter_sequence(&mut self) -> Result<(), NetError> {
        let (_tag, content_start, content_end) = self.read_tlv()?;
        self.buf = self.buf.get(content_start..content_end).ok_or_else(bad_der)?;
        self.pos = 0;
        Ok(())
    }

    /// Read the SEQUENCE at the cursor and return a sub-cursor over its contents,
    /// advancing this cursor past the whole field.
    fn enter_field_sequence(&mut self) -> Result<DerCursor<'a>, NetError> {
        let (_tag, content_start, content_end) = self.read_tlv()?;
        let inner = self.buf.get(content_start..content_end).ok_or_else(bad_der)?;
        self.pos = content_end;
        Ok(DerCursor::new(inner))
    }

    fn read_tlv(&self) -> Result<(u8, usize, usize), NetError> {
        let tag = *self.buf.get(self.pos).ok_or_else(bad_der)?;
        let len_byte = *self.buf.get(self.pos + 1).ok_or_else(bad_der)?;
        let (len, header) = if len_byte & 0x80 == 0 {
            (len_byte as usize, 2usize)
        } else {
            let n = (len_byte & 0x7f) as usize;
            if n == 0 || n > 4 {
                return Err(bad_der());
            }
            let mut len = 0usize;
            for i in 0..n {
                let b = *self.buf.get(self.pos + 2 + i).ok_or_else(bad_der)?;
                len = (len << 8) | b as usize;
            }
            (len, 2 + n)
        };
        let content_start = self.pos + header;
        let content_end = content_start.checked_add(len).ok_or_else(bad_der)?;
        if content_end > self.buf.len() {
            return Err(bad_der());
        }
        Ok((tag, content_start, content_end))
    }

    fn skip_field(&mut self) -> Result<(), NetError> {
        let (_tag, _start, end) = self.read_tlv()?;
        self.pos = end;
        Ok(())
    }

    /// Read the content bytes of the field at the cursor, advancing past it.
    fn read_field_content(&mut self) -> Result<&'a [u8], NetError> {
        let (_tag, start, end) = self.read_tlv()?;
        let content = self.buf.get(start..end).ok_or_else(bad_der)?;
        self.pos = end;
        Ok(content)
    }

    fn skip_optional_context0(&mut self) -> Result<(), NetError> {
        if *self.buf.get(self.pos).ok_or_else(bad_der)? == 0xA0 {
            self.skip_field()?;
        }
        Ok(())
    }
}

fn bad_der() -> NetError {
    NetError::Identity("malformed certificate DER".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_id_matches_cert_spki() {
        let id = DeviceIdentity::generate();
        let tls = build_tls_certificate(&id).expect("cert");
        let from_cert = device_id_from_cert(&tls.cert).expect("cert id");
        assert_eq!(
            from_cert,
            id.device_id(),
            "SHA-256(cert SPKI) must equal device_id (§3)"
        );
    }

    #[test]
    fn cert_path_matches_core_raw_key_path() {
        // The cert-extraction path must agree byte-for-byte with the core raw-key path.
        let id = DeviceIdentity::generate();
        let tls = build_tls_certificate(&id).expect("cert");
        let via_cert = device_id_from_cert(&tls.cert).expect("cert id");
        let via_core =
            device_id_from_public_key_bytes(&id.public_key_bytes()).expect("core derivation");
        assert_eq!(via_cert, via_core);
    }

    #[test]
    fn malformed_cert_is_rejected() {
        let der = CertificateDer::from(vec![0x30, 0x01, 0x00]);
        assert!(device_id_from_cert(&der).is_err());
    }
}
