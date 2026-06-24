//! mouser-net — the QUIC transport + mDNS discovery layer (docs/communication-interface.md
//! §0, §2, §4, §6, §7.6; docs/tech-stack.md §2).
//!
//! This crate implements the transport layer:
//! - [`discovery`] — advertise/browse `_mouser._udp.local` over mDNS (§4).
//! - [`identity`] — the TLS leaf cert built from the [`mouser_core::DeviceIdentity`]
//!   key, plus `device_id_from_cert` which feeds the single `mouser-core` SPKI→hash
//!   derivation (§3). The identity itself lives in `mouser-core`.
//! - [`pin`] — `device_id`-pinning rustls cert verifiers (§3).
//! - [`tls`] — TLS 1.3 rustls configs with ALPN `mouser/1` (§2).
//! - [`motion`] — app-level keep-newest pointer-motion datagram sender (§8/§7.6).
//! - [`transport`] — a `quinn` interactive connection: long-lived control stream
//!   (framed CBOR, §0.2/§6.1), §5 `Hello`/`HelloAck` channel binding, and RFC 9221
//!   datagram plane for `PointerMotion` (§7.6).
//! - [`bulk`] — the second QUIC connection (§6.2): `BulkHello` binding to the
//!   interactive session (§5 step 5) + one dedicated framed stream per `transfer_id`,
//!   reusing the interactive plane's cert/pin/TLS builders.
//! - [`sas`] — the mandatory §5 step-3 Short Authentication String: both ends derive an
//!   identical 6-digit code from the interactive TLS exporter + ascending-id context for
//!   the user to compare. Exposed as [`InteractiveConnection::sas`].
//!
//! Cert pinning (§3) is enforced on both planes. Interactive connections are returned
//! only after `channel_sig` verifies against the pinned leaf cert key; first-contact
//! human trust approval is enforced by the daemon before it starts runtime traffic.

// §0.3 panic-free decode discipline: the decode/runtime path must never panic.
// Decoders use checked slicing + `try_into` and return `NetError` instead.
// (The unwrap/panic/indexing denies come from `[workspace.lints.clippy]`.)

pub mod bulk;
mod control;
mod dial;
pub mod discovery;
mod endpoint_bind;
mod handshake;
pub mod identity;
pub mod motion;
pub mod pin;
pub mod sas;
pub mod tls;
pub mod transport;

pub use bulk::{BulkConnection, BulkEndpoint, TransferStream};
pub use discovery::{Advertiser, Browser, Discovery, PeerAdvert, PeerEvent, SERVICE_TYPE};
pub use endpoint_bind::{client_bind_for, dual_stack_addr, loopback_addr};
pub use identity::{
    build_tls_certificate, device_id_from_cert, verifying_key_from_cert, TlsCertificate,
};
pub use motion::{MotionPlane, MotionSender};
pub use mouser_core::{DeviceId, DeviceIdentity};
pub use pin::{DeviceIdPinVerifier, PinPolicy};
pub use sas::compute_sas;
pub use tls::ALPN_MOUSER_1;
pub use transport::{InteractiveConnection, InteractiveEndpoint};

/// Errors surfaced by the transport and discovery layers.
#[derive(Debug, thiserror::Error)]
pub enum NetError {
    /// Identity-key, cert, or `device_id` derivation failure (§3).
    #[error("identity: {0}")]
    Identity(String),
    /// rustls/TLS configuration failure (§2, §3).
    #[error("tls: {0}")]
    Tls(String),
    /// Socket/IO failure binding or driving an endpoint.
    #[error("io: {0}")]
    Io(String),
    /// QUIC connection/handshake failure (§5, §6).
    #[error("connect: {0}")]
    Connect(String),
    /// Control-stream framing failure (§0.2).
    #[error("frame: {0}")]
    Frame(String),
    /// Datagram (de)serialization or send/receive failure (§7.6).
    #[error("datagram: {0}")]
    Datagram(String),
    /// mDNS discovery failure (§4).
    #[error("discovery: {0}")]
    Discovery(String),
}
