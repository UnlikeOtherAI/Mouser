//! Certificate-pinning verifiers (§3). On **every** connection the receiver verifies
//! `SHA-256(presented_cert_SPKI) == expected device_id` before any `Hello` is
//! processed, via custom `rustls` `ServerCertVerifier` / `ClientCertVerifier`.
//!
//! Pinning mode:
//! - [`PinPolicy::Pinned`] — compare against a known `device_id` (the resume/§3 path).
//! - [`PinPolicy::TrustOnFirstUse`] — accept any well-formed identity cert and report
//!   the observed `device_id` (first-contact; SAS/pairing — STUBBED — would gate trust).
//!
//! The TLS *signature* is always cryptographically verified with the ring provider's
//! webpki algorithms; only the trust decision differs by policy.

use std::sync::Arc;

use mouser_core::DeviceId;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{verify_tls12_signature, verify_tls13_signature, WebPkiSupportedAlgorithms};
use rustls::server::danger::{ClientCertVerified, ClientCertVerifier};
use rustls::{
    crypto::ring::default_provider, CertificateError, DigitallySignedStruct, DistinguishedName,
    Error as TlsError, SignatureScheme,
};
use rustls_pki_types::{CertificateDer, ServerName, UnixTime};

use crate::identity::device_id_from_cert;

/// How a verifier decides whether a presented identity cert is trusted (§3, §5).
#[derive(Clone)]
pub enum PinPolicy {
    /// Require `SHA-256(cert SPKI) == device_id` (trusted-peer resume path, §3).
    Pinned(DeviceId),
    /// Accept any well-formed identity cert (first-contact). Pairing/SAS — STUBBED —
    /// would gate trust on the reported `device_id` before exchanging input/state.
    TrustOnFirstUse,
}

/// Shared `device_id`-pinning verifier used for both the server- and client-cert
/// directions (§3 mandates the check on both ends).
#[derive(Debug)]
pub struct DeviceIdPinVerifier {
    policy_pinned: Option<DeviceId>,
    algs: WebPkiSupportedAlgorithms,
    empty_hints: Vec<DistinguishedName>,
}

impl DeviceIdPinVerifier {
    /// Build a verifier from a [`PinPolicy`].
    pub fn new(policy: PinPolicy) -> Arc<Self> {
        let policy_pinned = match policy {
            PinPolicy::Pinned(id) => Some(id),
            PinPolicy::TrustOnFirstUse => None,
        };
        Arc::new(Self {
            policy_pinned,
            algs: default_provider().signature_verification_algorithms,
            empty_hints: Vec::new(),
        })
    }

    /// Enforce the §3 pin: `SHA-256(presented SPKI)` must equal the pinned id, or be
    /// accepted under trust-on-first-use.
    fn check_pin(&self, end_entity: &CertificateDer<'_>) -> Result<(), TlsError> {
        let observed = device_id_from_cert(end_entity)
            .map_err(|_| TlsError::InvalidCertificate(CertificateError::BadEncoding))?;
        match self.policy_pinned {
            Some(expected) if expected != observed => Err(TlsError::InvalidCertificate(
                CertificateError::ApplicationVerificationFailure,
            )),
            _ => Ok(()),
        }
    }
}

impl ServerCertVerifier for DeviceIdPinVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, TlsError> {
        self.check_pin(end_entity)?;
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, TlsError> {
        verify_tls12_signature(message, cert, dss, &self.algs)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, TlsError> {
        verify_tls13_signature(message, cert, dss, &self.algs)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.algs.supported_schemes()
    }
}

impl ClientCertVerifier for DeviceIdPinVerifier {
    fn root_hint_subjects(&self) -> &[DistinguishedName] {
        &self.empty_hints
    }

    fn verify_client_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _now: UnixTime,
    ) -> Result<ClientCertVerified, TlsError> {
        self.check_pin(end_entity)?;
        Ok(ClientCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, TlsError> {
        verify_tls12_signature(message, cert, dss, &self.algs)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, TlsError> {
        verify_tls13_signature(message, cert, dss, &self.algs)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.algs.supported_schemes()
    }
}
