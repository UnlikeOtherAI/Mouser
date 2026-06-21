//! QUIC transport (§6). A [`quinn`] endpoint speaking ALPN `mouser/1` over TLS 1.3,
//! with the leaf cert pinned to the peer `device_id` (§3). The **interactive
//! connection** (§6.1) carries:
//!
//! - one long-lived **bidi control stream**, framed per §0.2 (CBOR payloads); and
//! - an unreliable **DATAGRAM** plane (RFC 9221) carrying `PointerMotion` (§7.6).
//!
//! Hello / SAS pairing / `channel_sig` (§5) are **STUBBED** here: the control stream
//! and datagram plane are wired and round-trip, but no `Hello` handshake, no SAS, and
//! no identity-proof signature are exchanged yet. Cert pinning (§3) *is* enforced.

use std::net::SocketAddr;
use std::sync::Arc;

use bytes::Bytes;
use quinn::crypto::rustls::{QuicClientConfig, QuicServerConfig};
use quinn::{ClientConfig, Connection, Endpoint, RecvStream, SendStream, ServerConfig};
use tokio::sync::{Mutex, OnceCell};

use crate::identity::{Identity, TlsCertificate};
use crate::pin::PinPolicy;
use crate::{tls, NetError};

/// Which end of the interactive connection a peer is — determines who *opens* the
/// long-lived control stream (the initiator) and who *accepts* it (§6.1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Role {
    Initiator,
    Acceptor,
}

/// Loopback wildcard bind address (port 0 → OS-assigned) for an interactive plane.
pub fn loopback_addr() -> SocketAddr {
    SocketAddr::from(([127, 0, 0, 1], 0))
}

/// A bound QUIC endpoint that can accept inbound interactive connections and dial out.
pub struct InteractiveEndpoint {
    endpoint: Endpoint,
}

impl InteractiveEndpoint {
    /// Bind a server endpoint at `addr` presenting `identity`'s cert and pinning the
    /// dialing peer's cert per `peer_policy` (§3).
    pub fn bind_server(
        identity: &Identity,
        addr: SocketAddr,
        peer_policy: PinPolicy,
    ) -> Result<Self, NetError> {
        let cert = identity.tls_certificate()?;
        let server_config = build_server_config(&cert, peer_policy)?;
        let endpoint =
            Endpoint::server(server_config, addr).map_err(|e| NetError::Io(e.to_string()))?;
        Ok(Self { endpoint })
    }

    /// Bind a client-only endpoint (no listener) for dialing peers.
    pub fn bind_client(addr: SocketAddr) -> Result<Self, NetError> {
        let endpoint = Endpoint::client(addr).map_err(|e| NetError::Io(e.to_string()))?;
        Ok(Self { endpoint })
    }

    /// The locally bound socket address (resolves the OS-assigned port).
    pub fn local_addr(&self) -> Result<SocketAddr, NetError> {
        self.endpoint
            .local_addr()
            .map_err(|e| NetError::Io(e.to_string()))
    }

    /// Dial `addr` and *initiate* the interactive control stream (§6.1). The caller's
    /// `identity` cert is presented; the server's cert is pinned per `peer_policy`.
    pub async fn connect_interactive(
        &self,
        identity: &Identity,
        addr: SocketAddr,
        peer_policy: PinPolicy,
    ) -> Result<InteractiveConnection, NetError> {
        let cert = identity.tls_certificate()?;
        let client_config = build_client_config(&cert, peer_policy)?;
        let connecting = self
            .endpoint
            .connect_with(client_config, addr, "mouser")
            .map_err(|e| NetError::Connect(e.to_string()))?;
        let connection = connecting
            .await
            .map_err(|e| NetError::Connect(e.to_string()))?;
        // The initiator opens the single long-lived control stream lazily on first use
        // (§6.1) — see `InteractiveConnection`. Doing it lazily avoids a handshake-time
        // deadlock, since a freshly-opened bidi stream only materializes on the peer
        // once data is written to it.
        Ok(InteractiveConnection::new(connection, Role::Initiator))
    }

    /// Accept the next inbound interactive connection (§6.1). The control stream is
    /// accepted lazily on first use.
    pub async fn accept_interactive(&self) -> Result<InteractiveConnection, NetError> {
        let incoming = self
            .endpoint
            .accept()
            .await
            .ok_or_else(|| NetError::Connect("endpoint closed".to_string()))?;
        let connection = incoming
            .await
            .map_err(|e| NetError::Connect(e.to_string()))?;
        Ok(InteractiveConnection::new(connection, Role::Acceptor))
    }

    /// Close the endpoint, terminating all connections.
    pub fn close(&self) {
        self.endpoint.close(0u32.into(), b"shutdown");
    }
}

fn build_server_config(
    cert: &TlsCertificate,
    peer_policy: PinPolicy,
) -> Result<ServerConfig, NetError> {
    let rustls_config = tls::server_config(cert, peer_policy)?;
    let quic =
        QuicServerConfig::try_from(rustls_config).map_err(|e| NetError::Tls(e.to_string()))?;
    Ok(ServerConfig::with_crypto(Arc::new(quic)))
}

fn build_client_config(
    cert: &TlsCertificate,
    peer_policy: PinPolicy,
) -> Result<ClientConfig, NetError> {
    let rustls_config = tls::client_config(cert, peer_policy)?;
    let quic =
        QuicClientConfig::try_from(rustls_config).map_err(|e| NetError::Tls(e.to_string()))?;
    Ok(ClientConfig::new(Arc::new(quic)))
}

/// An established interactive connection (§6.1): a long-lived bidi control stream
/// plus the QUIC datagram plane for pointer motion. The control stream is established
/// lazily on first send/recv — the initiator `open_bi`s it, the acceptor `accept_bi`s it.
pub struct InteractiveConnection {
    connection: Connection,
    role: Role,
    control: OnceCell<ControlStream>,
}

struct ControlStream {
    send: Mutex<SendStream>,
    recv: Mutex<RecvStream>,
}

impl InteractiveConnection {
    fn new(connection: Connection, role: Role) -> Self {
        Self {
            connection,
            role,
            control: OnceCell::new(),
        }
    }

    /// Lazily establish the long-lived bidi control stream (§6.1), opening or accepting
    /// it depending on this peer's role.
    async fn control(&self) -> Result<&ControlStream, NetError> {
        self.control
            .get_or_try_init(|| async {
                let (send, recv) = match self.role {
                    Role::Initiator => self
                        .connection
                        .open_bi()
                        .await
                        .map_err(|e| NetError::Connect(e.to_string()))?,
                    Role::Acceptor => self
                        .connection
                        .accept_bi()
                        .await
                        .map_err(|e| NetError::Connect(e.to_string()))?,
                };
                Ok(ControlStream {
                    send: Mutex::new(send),
                    recv: Mutex::new(recv),
                })
            })
            .await
    }

    /// The negotiated ALPN (should be [`tls::ALPN_MOUSER_1`]) — proves §2 versioning.
    pub fn negotiated_alpn(&self) -> Option<Vec<u8>> {
        self.connection
            .handshake_data()
            .and_then(|d| d.downcast::<quinn::crypto::rustls::HandshakeData>().ok())
            .and_then(|d| d.protocol)
    }

    /// The peer's pinned `device_id` derived from its presented leaf cert (§3).
    pub fn peer_device_id(&self) -> Option<[u8; 32]> {
        let identity = self.connection.peer_identity()?;
        let certs = identity
            .downcast::<Vec<rustls_pki_types::CertificateDer<'static>>>()
            .ok()?;
        let leaf = certs.first()?;
        crate::identity::device_id_from_cert(leaf).ok()
    }

    /// Send a framed control message (§0.2): `encode_frame(msg_type, 0, payload)` on
    /// the control stream. `payload` is the CBOR body produced by `mouser-protocol`.
    pub async fn send_control(&self, msg_type: u16, payload: &[u8]) -> Result<(), NetError> {
        let frame = mouser_protocol::encode_frame(msg_type, 0, payload)
            .map_err(|e| NetError::Frame(e.to_string()))?;
        let control = self.control().await?;
        let mut send = control.send.lock().await;
        send.write_all(&frame)
            .await
            .map_err(|e| NetError::Io(e.to_string()))?;
        Ok(())
    }

    /// Receive one framed control message, returning `(msg_type, payload_bytes)`.
    /// Reads the 8-byte header, then the declared payload (§0.2).
    pub async fn recv_control(&self) -> Result<(u16, Vec<u8>), NetError> {
        let control = self.control().await?;
        let mut recv = control.recv.lock().await;
        let mut header = [0u8; 8];
        recv.read_exact(&mut header)
            .await
            .map_err(|e| NetError::Io(e.to_string()))?;
        // Decode the §0.2 header with checked slicing (no panicking index — §0.3
        // panic-free decode discipline): len: u32 (LE) | type: u16 (LE) | ...
        let len_bytes: [u8; 4] = header
            .get(0..4)
            .and_then(|b| b.try_into().ok())
            .ok_or_else(|| NetError::Frame("short control frame header".to_string()))?;
        let type_bytes: [u8; 2] = header
            .get(4..6)
            .and_then(|b| b.try_into().ok())
            .ok_or_else(|| NetError::Frame("short control frame header".to_string()))?;
        let len = u32::from_le_bytes(len_bytes);
        let msg_type = u16::from_le_bytes(type_bytes);
        if !(4..=mouser_protocol::MAX_CONTROL_FRAME).contains(&len) {
            return Err(NetError::Frame(
                "control frame length out of range".to_string(),
            ));
        }
        let payload_len = (len - 4) as usize;
        let mut payload = vec![0u8; payload_len];
        recv.read_exact(&mut payload)
            .await
            .map_err(|e| NetError::Io(e.to_string()))?;
        Ok((msg_type, payload))
    }

    /// Send a `PointerMotion` as an unreliable QUIC datagram (§7.6 tag 0x01).
    pub fn send_motion(&self, motion: &mouser_protocol::PointerMotion) -> Result<(), NetError> {
        let bytes = mouser_protocol::encode_motion(motion)
            .map_err(|e| NetError::Datagram(e.to_string()))?;
        self.connection
            .send_datagram(Bytes::from(bytes))
            .map_err(|e| NetError::Datagram(e.to_string()))
    }

    /// Receive and decode the next motion datagram (§7.6). An unknown tag yields
    /// [`mouser_protocol::Datagram::Unknown`] (the caller drops it).
    pub async fn recv_motion(&self) -> Result<mouser_protocol::Datagram, NetError> {
        let bytes = self
            .connection
            .read_datagram()
            .await
            .map_err(|e| NetError::Datagram(e.to_string()))?;
        mouser_protocol::decode_datagram(&bytes).map_err(|e| NetError::Datagram(e.to_string()))
    }

    /// Close the connection cleanly.
    pub fn close(&self) {
        self.connection.close(0u32.into(), b"bye");
    }
}
