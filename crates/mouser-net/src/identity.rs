//! Device identity (§3). A permanent **Ed25519 keypair** is generated on first
//! launch; `device_id = SHA-256(SubjectPublicKeyInfo)` of the Ed25519 public key
//! (the full 32 bytes used for all pinning/comparison). The **TLS leaf certificate's
//! public key IS the identity key** — `rcgen` builds a self-signed cert *from* the
//! identity keypair so `SHA-256(cert SPKI) == device_id` holds by construction.

use ed25519_dalek::pkcs8::{EncodePrivateKey, EncodePublicKey};
use ed25519_dalek::SigningKey;
use rcgen::{CertificateParams, DistinguishedName, DnType, KeyPair, PKCS_ED25519};
use rustls_pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use sha2::{Digest, Sha256};

use crate::NetError;

/// A device's permanent Ed25519 identity and its derived `device_id`.
pub struct Identity {
    signing: SigningKey,
    device_id: [u8; 32],
}

impl Identity {
    /// Generate a fresh random identity (first-launch path, §3).
    pub fn generate() -> Result<Self, NetError> {
        let signing = SigningKey::generate(&mut rand_core::OsRng);
        Self::from_signing_key(signing)
    }

    /// Build an identity from an existing Ed25519 signing key (persisted on disk
    /// in production; supplied directly in tests).
    pub fn from_signing_key(signing: SigningKey) -> Result<Self, NetError> {
        let device_id = device_id_from_verifying(&signing)?;
        Ok(Self { signing, device_id })
    }

    /// The full 32-byte `device_id` (SHA-256 of the Ed25519 SPKI, §3).
    pub fn device_id(&self) -> [u8; 32] {
        self.device_id
    }

    /// Lowercase base32 (no padding) rendering of `device_id` for the mDNS `id`
    /// TXT key (§4). Display/advisory only — never used for trust comparison.
    pub fn device_id_b32(&self) -> String {
        base32_lower_nopad(&self.device_id)
    }

    /// Borrow the underlying signing key (used for the §5 `channel_sig` proof,
    /// which is stubbed in this skeleton).
    pub fn signing_key(&self) -> &SigningKey {
        &self.signing
    }

    /// Build a self-signed TLS leaf cert whose public key is this identity key,
    /// returning the DER cert chain and the PKCS#8 private key for rustls.
    pub fn tls_certificate(&self) -> Result<TlsCertificate, NetError> {
        let pkcs8 = self
            .signing
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
}

/// A self-signed TLS leaf certificate + its private key, both DER-encoded.
pub struct TlsCertificate {
    /// The DER leaf certificate (its SPKI is the identity key, §3).
    pub cert: CertificateDer<'static>,
    /// The PKCS#8 DER private key.
    pub key: PrivateKeyDer<'static>,
}

/// Compute `device_id = SHA-256(SubjectPublicKeyInfo)` for a signing key's public
/// half (§3). Identical whether derived from the raw identity key or the leaf cert.
fn device_id_from_verifying(signing: &SigningKey) -> Result<[u8; 32], NetError> {
    let spki = signing
        .verifying_key()
        .to_public_key_der()
        .map_err(|e| NetError::Identity(e.to_string()))?;
    let digest = Sha256::digest(spki.as_bytes());
    Ok(digest.into())
}

/// Compute `device_id` from a presented leaf certificate's SPKI (§3) — the form a
/// cert verifier uses to check `SHA-256(presented_cert_SPKI) == pinned device_id`.
pub fn device_id_from_cert(cert: &CertificateDer<'_>) -> Result<[u8; 32], NetError> {
    let spki = spki_from_cert(cert.as_ref())?;
    Ok(Sha256::digest(spki).into())
}

/// Extract the raw 32-byte Ed25519 public key from a presented leaf certificate's
/// SPKI. The leaf's public key **is** the device identity key (§3), so this is the
/// verifying key for the §5 `channel_sig` proof — letting the bulk plane (`crate::bulk`)
/// check the binding signature against the same key it pins, with no extra wire field.
pub fn verifying_key_from_cert(
    cert: &CertificateDer<'_>,
) -> Result<ed25519_dalek::VerifyingKey, NetError> {
    let spki = spki_from_cert(cert.as_ref())?;
    // SPKI = SEQUENCE { AlgorithmIdentifier, subjectPublicKey BIT STRING }. Walk to the
    // BIT STRING, drop its leading "unused bits" octet, and the remainder is the key.
    let mut p = DerCursor::new(&spki);
    p.enter_sequence()?; // SubjectPublicKeyInfo
    p.skip_field()?; // algorithm AlgorithmIdentifier
    let bit_string = p.read_field_raw()?; // subjectPublicKey BIT STRING (with TLV header)
    // BIT STRING content is `<unused-bits octet><key bytes>`; for Ed25519 unused=0 and
    // key is 32 bytes. The raw field still carries its tag+len, so locate the content.
    let key = ed25519_bytes_from_bit_string(bit_string)?;
    ed25519_dalek::VerifyingKey::from_bytes(&key)
        .map_err(|e| NetError::Identity(format!("bad Ed25519 key: {e}")))
}

/// Pull the trailing 32 key bytes out of a DER BIT STRING field (`03 LEN 00 <key>`).
fn ed25519_bytes_from_bit_string(field: &[u8]) -> Result<[u8; 32], NetError> {
    // Smallest valid encoding here is `03 21 00 <32 bytes>` = 35 bytes.
    if field.len() < 3 || field.first().copied() != Some(0x03) {
        return Err(bad_der());
    }
    let key = field.get(field.len() - 32..).ok_or(bad_der())?;
    key.try_into().map_err(|_| bad_der())
}

/// Extract the DER `SubjectPublicKeyInfo` bytes from a DER certificate. Parses the
/// X.509 `TBSCertificate` far enough to locate the `subjectPublicKeyInfo` field
/// without pulling in a full ASN.1 dependency.
fn spki_from_cert(der: &[u8]) -> Result<Vec<u8>, NetError> {
    let mut p = DerCursor::new(der);
    p.enter_sequence()?; // Certificate
    p.enter_sequence()?; // TBSCertificate
    p.skip_optional_context0()?; // [0] version (optional)
    p.skip_field()?; // serialNumber
    p.skip_field()?; // signature AlgorithmIdentifier
    p.skip_field()?; // issuer
    p.skip_field()?; // validity
    p.skip_field()?; // subject
    let spki = p.read_field_raw()?; // subjectPublicKeyInfo
    Ok(spki.to_vec())
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

    fn enter_sequence(&mut self) -> Result<(), NetError> {
        let (_tag, content_start, content_end) = self.read_tlv()?;
        self.buf = self.buf.get(content_start..content_end).ok_or(bad_der())?;
        self.pos = 0;
        Ok(())
    }

    fn read_tlv(&self) -> Result<(u8, usize, usize), NetError> {
        let tag = *self.buf.get(self.pos).ok_or(bad_der())?;
        let len_byte = *self.buf.get(self.pos + 1).ok_or(bad_der())?;
        let (len, header) = if len_byte & 0x80 == 0 {
            (len_byte as usize, 2usize)
        } else {
            let n = (len_byte & 0x7f) as usize;
            if n == 0 || n > 4 {
                return Err(bad_der());
            }
            let mut len = 0usize;
            for i in 0..n {
                let b = *self.buf.get(self.pos + 2 + i).ok_or(bad_der())?;
                len = (len << 8) | b as usize;
            }
            (len, 2 + n)
        };
        let content_start = self.pos + header;
        let content_end = content_start.checked_add(len).ok_or(bad_der())?;
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

    fn read_field_raw(&mut self) -> Result<&'a [u8], NetError> {
        let start = self.pos;
        let (_tag, _content_start, end) = self.read_tlv()?;
        let raw = self.buf.get(start..end).ok_or(bad_der())?;
        self.pos = end;
        Ok(raw)
    }

    fn skip_optional_context0(&mut self) -> Result<(), NetError> {
        if *self.buf.get(self.pos).ok_or(bad_der())? == 0xA0 {
            self.skip_field()?;
        }
        Ok(())
    }
}

fn bad_der() -> NetError {
    NetError::Identity("malformed certificate DER".to_string())
}

/// RFC 4648 base32 lowercase, no padding (mDNS `id` TXT key, §4).
fn base32_lower_nopad(data: &[u8]) -> String {
    const ALPHABET: &[u8; 32] = b"abcdefghijklmnopqrstuvwxyz234567";
    let mut out = String::with_capacity(data.len().div_ceil(5) * 8);
    let mut buffer = 0u32;
    let mut bits = 0u32;
    for &byte in data {
        buffer = (buffer << 8) | byte as u32;
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            let idx = ((buffer >> bits) & 0x1f) as usize;
            out.push(ALPHABET.get(idx).copied().unwrap_or(b'a') as char);
        }
    }
    if bits > 0 {
        let idx = ((buffer << (5 - bits)) & 0x1f) as usize;
        out.push(ALPHABET.get(idx).copied().unwrap_or(b'a') as char);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_id_matches_cert_spki() {
        let id = Identity::generate().expect("identity");
        let tls = id.tls_certificate().expect("cert");
        let from_cert = device_id_from_cert(&tls.cert).expect("cert id");
        assert_eq!(
            from_cert,
            id.device_id(),
            "SHA-256(cert SPKI) must equal device_id (§3)"
        );
    }

    #[test]
    fn device_id_is_stable_for_same_key() {
        let signing = SigningKey::generate(&mut rand_core::OsRng);
        let a = Identity::from_signing_key(signing.clone()).expect("a");
        let b = Identity::from_signing_key(signing).expect("b");
        assert_eq!(a.device_id(), b.device_id());
    }

    #[test]
    fn base32_render_is_lowercase_nopad() {
        const ALPHABET: &str = "abcdefghijklmnopqrstuvwxyz234567";
        let s = base32_lower_nopad(&[0xff; 32]);
        assert!(s.chars().all(|c| ALPHABET.contains(c)));
        assert!(!s.contains('='));
    }
}
