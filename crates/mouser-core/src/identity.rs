//! Device identity (spec §3).
//!
//! A Mouser device has a permanent **Ed25519 keypair** generated on first launch.
//! Its `device_id` is `SHA-256(SubjectPublicKeyInfo)` — the SHA-256 over the DER
//! `SubjectPublicKeyInfo` of the Ed25519 public key. For Ed25519 the SPKI is a fixed
//! 44-byte structure: a 12-byte prefix
//! (`30 2a 30 05 06 03 2b 65 70 03 21 00`) followed by the 32-byte raw public key.
//! Because the TLS leaf certificate carries this same key, the `device_id` computed
//! here is identical to `SHA-256(presented_cert_SPKI)` — the value pinned on every
//! connection (spec §3).
//!
//! The 32-byte `device_id` is the sole basis for comparison and pinning. The base32
//! [`DeviceIdentity::device_id_base32`] rendering is **display-only** and never
//! compared.

use data_encoding::BASE32_NOPAD;
use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey, PUBLIC_KEY_LENGTH};
use sha2::{Digest, Sha256};

use crate::DeviceId;

/// The 12-byte DER `SubjectPublicKeyInfo` prefix that precedes the 32-byte raw
/// Ed25519 public key. Together they form the 44-byte SPKI hashed for `device_id`.
///
/// Bytes: `SEQUENCE { SEQUENCE { OID 1.3.101.112 } BIT STRING (0 unused bits) }`.
const ED25519_SPKI_PREFIX: [u8; 12] = [
    0x30, 0x2a, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x03, 0x21, 0x00,
];

/// Length of the DER SPKI for an Ed25519 key: 12-byte prefix + 32-byte key.
pub const ED25519_SPKI_LEN: usize = ED25519_SPKI_PREFIX.len() + PUBLIC_KEY_LENGTH;

/// Errors building a verifying key from external bytes (e.g. a peer's raw public
/// key at cert-pinning time).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IdentityError {
    /// The supplied bytes are not a valid Ed25519 public key (off-curve / bad encoding).
    InvalidPublicKey,
}

impl core::fmt::Display for IdentityError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            IdentityError::InvalidPublicKey => f.write_str("invalid Ed25519 public key"),
        }
    }
}

impl std::error::Error for IdentityError {}

/// Build the 44-byte DER `SubjectPublicKeyInfo` for a raw Ed25519 public key.
fn ed25519_spki(raw_public_key: &[u8; PUBLIC_KEY_LENGTH]) -> [u8; ED25519_SPKI_LEN] {
    let mut spki = [0u8; ED25519_SPKI_LEN];
    let (prefix, key) = spki.split_at_mut(ED25519_SPKI_PREFIX.len());
    prefix.copy_from_slice(&ED25519_SPKI_PREFIX);
    key.copy_from_slice(raw_public_key);
    spki
}

/// Compute `device_id = SHA-256(SubjectPublicKeyInfo)` for a verifying key (spec §3).
///
/// This is exposed as a free function so connection-time cert pinning can reuse the
/// exact derivation against a presented certificate's key.
pub fn device_id_from_public_key(public_key: &VerifyingKey) -> DeviceId {
    let spki = ed25519_spki(public_key.as_bytes());
    let mut hasher = Sha256::new();
    hasher.update(spki);
    hasher.finalize().into()
}

/// Compute a peer's `device_id` from its raw 32-byte Ed25519 public key, validating
/// the key first (spec §3 cert-pinning derivation). Returns
/// [`IdentityError::InvalidPublicKey`] for an off-curve / malformed key.
pub fn device_id_from_public_key_bytes(
    raw_public_key: &[u8; PUBLIC_KEY_LENGTH],
) -> Result<DeviceId, IdentityError> {
    let public_key =
        VerifyingKey::from_bytes(raw_public_key).map_err(|_| IdentityError::InvalidPublicKey)?;
    Ok(device_id_from_public_key(&public_key))
}

/// A device's permanent Ed25519 identity and its derived `device_id` (spec §3).
///
/// Construct a fresh one with [`DeviceIdentity::generate`]. The private key never
/// leaves this struct; only the public key, `device_id`, and signatures are exposed.
pub struct DeviceIdentity {
    signing_key: SigningKey,
    device_id: DeviceId,
}

impl DeviceIdentity {
    /// Generate a brand-new identity from the operating system CSPRNG.
    ///
    /// This is the "first launch" path. The resulting keypair is permanent for the
    /// life of the install and should be persisted by the caller (this crate does no
    /// I/O).
    pub fn generate() -> Self {
        let signing_key = SigningKey::generate(&mut rand_core::OsRng);
        Self::from_signing_key(signing_key)
    }

    /// Reconstruct an identity from a previously persisted 32-byte Ed25519 seed.
    ///
    /// The seed is the raw private scalar material as produced by
    /// [`DeviceIdentity::secret_seed`]; any 32 bytes are accepted as a valid seed.
    pub fn from_seed(seed: &[u8; 32]) -> Self {
        Self::from_signing_key(SigningKey::from_bytes(seed))
    }

    fn from_signing_key(signing_key: SigningKey) -> Self {
        let device_id = device_id_from_public_key(&signing_key.verifying_key());
        Self {
            signing_key,
            device_id,
        }
    }

    /// The Ed25519 public (verifying) key.
    pub fn public_key(&self) -> VerifyingKey {
        self.signing_key.verifying_key()
    }

    /// The 32-byte raw Ed25519 public key.
    pub fn public_key_bytes(&self) -> [u8; PUBLIC_KEY_LENGTH] {
        self.signing_key.verifying_key().to_bytes()
    }

    /// The 44-byte DER `SubjectPublicKeyInfo` whose SHA-256 is the `device_id`.
    pub fn spki(&self) -> [u8; ED25519_SPKI_LEN] {
        ed25519_spki(&self.public_key_bytes())
    }

    /// The permanent 32-byte `device_id` (spec §3) — the only value compared/pinned.
    pub fn device_id(&self) -> DeviceId {
        self.device_id
    }

    /// Display-only base32 (RFC 4648, no padding, lowercase) rendering of the full
    /// `device_id`. Never used for comparison (spec §3).
    pub fn device_id_base32(&self) -> String {
        device_id_base32(&self.device_id)
    }

    /// The 32-byte secret seed, for persistence by the caller. Treat as sensitive.
    pub fn secret_seed(&self) -> [u8; 32] {
        self.signing_key.to_bytes()
    }

    /// Sign a message with the identity key (used for the `channel_sig` proof, §5).
    pub fn sign(&self, message: &[u8]) -> Signature {
        self.signing_key.sign(message)
    }
}

impl core::fmt::Debug for DeviceIdentity {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        // Never print the secret key material.
        f.debug_struct("DeviceIdentity")
            .field("device_id", &device_id_base32(&self.device_id))
            .finish_non_exhaustive()
    }
}

/// Display-only base32 (RFC 4648, no padding, lowercase) rendering of a `device_id`.
pub fn device_id_base32(device_id: &DeviceId) -> String {
    BASE32_NOPAD.encode(device_id).to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Known-key vector: the **all-zero** 32-byte Ed25519 public key. SPKI = the
    /// 12-byte prefix + 32 zero bytes; `device_id` = SHA-256(SPKI). Ground truth
    /// computed independently (Python `hashlib`).
    #[test]
    fn known_zero_key_spki_and_device_id() {
        let raw = [0u8; PUBLIC_KEY_LENGTH];
        let spki = ed25519_spki(&raw);

        let expected_spki =
            "302a300506032b65700321000000000000000000000000000000000000000000000000000000000000000000";
        assert_eq!(hex(&spki), expected_spki);
        assert_eq!(spki.len(), ED25519_SPKI_LEN);

        let mut hasher = Sha256::new();
        hasher.update(spki);
        let device_id: DeviceId = hasher.finalize().into();
        assert_eq!(
            hex(&device_id),
            "722abd12e99a5367f375aeb9672a8e07712e03c2add16fa8d6914d1cfa2efe0c"
        );
        assert_eq!(
            device_id_base32(&device_id),
            "oivl2exjtjjwp43vv24wokuoa5ys4a6cvxiw7kgwsfgrz6ro7yga"
        );
    }

    /// Known-key vector derived through the public `VerifyingKey` path, using the
    /// RFC 8032 Ed25519 test-vector-1 public key. Confirms `device_id_from_public_key`
    /// produces the same SPKI hash as the manual derivation above.
    #[test]
    fn known_rfc8032_public_key_device_id() {
        let raw = hex_decode("d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a");
        let mut key_bytes = [0u8; PUBLIC_KEY_LENGTH];
        key_bytes.copy_from_slice(&raw);
        let public_key = VerifyingKey::from_bytes(&key_bytes).expect("valid test vector key");

        let device_id = device_id_from_public_key(&public_key);
        assert_eq!(
            hex(&device_id),
            "06e3fd8fda29bb60ab59557de61edb0aecdb231134be30e75b455f8e1b792fa9"
        );
        assert_eq!(
            device_id_base32(&device_id),
            "a3r73d62fg5wbk2zkv66mhw3blwnwiyrgs7dbz23ivpy4g3zf6uq"
        );
    }

    #[test]
    fn device_id_from_bytes_matches_and_validates() {
        // Valid raw key bytes derive the same device_id as the VerifyingKey path.
        let raw = hex_decode("d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a");
        let mut key_bytes = [0u8; PUBLIC_KEY_LENGTH];
        key_bytes.copy_from_slice(&raw);
        let device_id =
            device_id_from_public_key_bytes(&key_bytes).expect("valid key derives device_id");
        assert_eq!(
            hex(&device_id),
            "06e3fd8fda29bb60ab59557de61edb0aecdb231134be30e75b455f8e1b792fa9"
        );

        // An off-curve compressed point (y = 2) is not decompressible -> error.
        let mut off_curve = [0u8; PUBLIC_KEY_LENGTH];
        off_curve[0] = 0x02;
        assert_eq!(
            device_id_from_public_key_bytes(&off_curve),
            Err(IdentityError::InvalidPublicKey)
        );
    }

    #[test]
    fn spki_prefix_matches_spec() {
        // The 12-byte prefix from spec §3, byte-for-byte.
        assert_eq!(hex(&ED25519_SPKI_PREFIX), "302a300506032b6570032100");
    }

    #[test]
    fn generate_is_deterministic_round_trip() {
        let identity = DeviceIdentity::generate();
        // Re-deriving from the persisted seed yields the same device_id.
        let seed = identity.secret_seed();
        let restored = DeviceIdentity::from_seed(&seed);
        assert_eq!(identity.device_id(), restored.device_id());
        assert_eq!(identity.public_key_bytes(), restored.public_key_bytes());
    }

    #[test]
    fn device_id_matches_public_key_path() {
        let identity = DeviceIdentity::generate();
        let via_struct = identity.device_id();
        let via_fn = device_id_from_public_key(&identity.public_key());
        assert_eq!(via_struct, via_fn);
    }

    #[test]
    fn signatures_verify_against_public_key() {
        use ed25519_dalek::Verifier;
        let identity = DeviceIdentity::generate();
        let message = b"mouser-channel-v1 proof bytes";
        let signature = identity.sign(message);
        assert!(identity.public_key().verify(message, &signature).is_ok());
    }

    #[test]
    fn base32_is_lowercase_unpadded() {
        let identity = DeviceIdentity::generate();
        let rendered = identity.device_id_base32();
        assert!(!rendered.contains('='), "base32 must be unpadded");
        assert_eq!(
            rendered,
            rendered.to_lowercase(),
            "base32 must be lowercase"
        );
        // 32 bytes => 52 base32 chars (no padding).
        assert_eq!(rendered.len(), 52);
    }

    fn hex(bytes: &[u8]) -> String {
        let mut out = String::with_capacity(bytes.len() * 2);
        for byte in bytes {
            out.push_str(&format!("{byte:02x}"));
        }
        out
    }

    fn hex_decode(s: &str) -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("valid hex"))
            .collect()
    }
}
