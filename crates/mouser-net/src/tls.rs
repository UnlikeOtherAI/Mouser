//! rustls TLS 1.3 configuration for the Mouser QUIC planes (§2, §3). Both sides
//! present their identity cert and pin the peer's cert to its `device_id` via the
//! [`crate::pin`] verifiers. ALPN is `mouser/1` — the **sole** protocol-version
//! source (§2). The ring crypto provider is used (quinn `rustls-ring`).

use std::sync::Arc;

use rustls::version::TLS13;
use rustls::{ClientConfig, ServerConfig};

use crate::identity::TlsCertificate;
use crate::pin::{DeviceIdPinVerifier, PinPolicy};
use crate::NetError;

/// The Mouser ALPN token for protocol version 1 (§2). Advertised by both ends; TLS
/// selects the common maximum. This is the only version signal — no `Hello` field.
pub const ALPN_MOUSER_1: &[u8] = b"mouser/1";

/// Build a rustls [`ServerConfig`]: TLS 1.3 only, identity cert presented, peer
/// (client) cert pinned via `peer_policy`, ALPN = `mouser/1`.
pub fn server_config(
    cert: &TlsCertificate,
    peer_policy: PinPolicy,
) -> Result<ServerConfig, NetError> {
    let verifier = DeviceIdPinVerifier::new(peer_policy);
    let mut config = ServerConfig::builder_with_protocol_versions(&[&TLS13])
        .with_client_cert_verifier(verifier)
        .with_single_cert(vec![cert.cert.clone()], cert.key.clone_key())
        .map_err(|e| NetError::Tls(e.to_string()))?;
    config.alpn_protocols = vec![ALPN_MOUSER_1.to_vec()];
    Ok(config)
}

/// Build a rustls [`ClientConfig`]: TLS 1.3 only, server cert pinned via
/// `peer_policy`, own identity cert presented for mutual auth, ALPN = `mouser/1`.
pub fn client_config(
    cert: &TlsCertificate,
    peer_policy: PinPolicy,
) -> Result<ClientConfig, NetError> {
    let verifier: Arc<DeviceIdPinVerifier> = DeviceIdPinVerifier::new(peer_policy);
    let mut config = ClientConfig::builder_with_protocol_versions(&[&TLS13])
        .dangerous()
        .with_custom_certificate_verifier(verifier)
        .with_client_auth_cert(vec![cert.cert.clone()], cert.key.clone_key())
        .map_err(|e| NetError::Tls(e.to_string()))?;
    config.alpn_protocols = vec![ALPN_MOUSER_1.to_vec()];
    Ok(config)
}
