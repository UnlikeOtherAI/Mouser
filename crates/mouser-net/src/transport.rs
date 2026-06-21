//! QUIC transport (§6). A [`quinn`] endpoint speaking ALPN `mouser/1` over TLS 1.3,
//! with the leaf cert pinned to the peer `device_id` (§3). The **interactive
//! connection** (§6.1) carries:
//!
//! - one long-lived **bidi control stream**, framed per §0.2 (CBOR payloads); and
//! - an unreliable **DATAGRAM** plane (RFC 9221) carrying `PointerMotion` (§7.6).
//!
//! The control stream is established **eagerly and symmetrically** at connect/accept
//! (the initiator opens it and writes a priming frame to materialize it on the peer;
//! the acceptor accepts it and consumes the prime), so neither direction's first I/O
//! can deadlock (A2). A [`quinn::TransportConfig`] sets QUIC keep-alive + a bounded
//! idle timeout on both ends (H1).
//!
//! Hello / SAS pairing / `channel_sig` (§5) are **STUBBED** here: the control stream
//! and datagram plane are wired and round-trip, but no `Hello` handshake, no SAS, and
//! no identity-proof signature are exchanged yet. Cert pinning (§3) *is* enforced.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use mouser_core::{DeviceId, DeviceIdentity};
use quinn::crypto::rustls::{QuicClientConfig, QuicServerConfig};
use quinn::{
    ClientConfig, Connection, Endpoint, IdleTimeout, RecvStream, SendStream, ServerConfig,
    TransportConfig,
};
use tokio::sync::Mutex;

use crate::identity::{build_tls_certificate, device_id_from_cert, TlsCertificate};
use crate::motion::MotionSender;
use crate::pin::PinPolicy;
use crate::{tls, NetError};

/// QUIC keep-alive interval (H1): PING often enough to keep an idle interactive
/// connection alive through NAT/firewall idle timers and to surface a dead path fast.
const KEEP_ALIVE: Duration = Duration::from_secs(5);

/// QUIC max idle timeout (H1): comfortably larger than [`KEEP_ALIVE`] and the
/// engine's heartbeat window, so a transient stall doesn't tear down the connection.
const MAX_IDLE: Duration = Duration::from_secs(20);

/// Bound the quinn datagram send buffer (A4): the app-level keep-newest sender
/// ([`crate::motion`]) already coalesces, so only a couple of frames need to queue.
/// 4 KiB is a handful of motion datagrams.
const DATAGRAM_SEND_BUFFER: usize = 4 * 1024;

/// Reserved control-frame type for the stream-priming frame (A2). It carries an empty
/// payload and is consumed during connection setup; it is never surfaced to callers.
const TYPE_STREAM_PRIME: u16 = 0xFFFF;

/// Which end of the interactive connection a peer is — determines who *opens* the
/// long-lived control stream (the initiator) and who *accepts* it (§6.1). Named
/// `ConnectionEnd` so the wire-level eligibility `Role` (mouser-protocol) is unambiguous.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ConnectionEnd {
    Initiator,
    Acceptor,
}

/// Loopback wildcard bind address (port 0 → OS-assigned) for an interactive plane.
pub fn loopback_addr() -> SocketAddr {
    SocketAddr::from(([127, 0, 0, 1], 0))
}

/// Build the shared [`TransportConfig`] applied to both endpoints (H1, A4).
fn transport_config() -> Result<Arc<TransportConfig>, NetError> {
    let mut cfg = TransportConfig::default();
    let idle = IdleTimeout::try_from(MAX_IDLE).map_err(|e| NetError::Tls(e.to_string()))?;
    cfg.max_idle_timeout(Some(idle));
    cfg.keep_alive_interval(Some(KEEP_ALIVE));
    cfg.datagram_send_buffer_size(DATAGRAM_SEND_BUFFER);
    Ok(Arc::new(cfg))
}

/// A bound QUIC endpoint that can accept inbound interactive connections and dial out.
pub struct InteractiveEndpoint {
    endpoint: Endpoint,
}

impl InteractiveEndpoint {
    /// Bind a server endpoint at `addr` presenting `identity`'s cert and pinning the
    /// dialing peer's cert per `peer_policy` (§3).
    pub fn bind_server(
        identity: &DeviceIdentity,
        addr: SocketAddr,
        peer_policy: PinPolicy,
    ) -> Result<Self, NetError> {
        let cert = build_tls_certificate(identity)?;
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
    /// `identity` cert is presented; the server's cert is pinned per `peer_policy`. The
    /// control stream is opened and primed before returning, so it cannot deadlock (A2).
    pub async fn connect_interactive(
        &self,
        identity: &DeviceIdentity,
        addr: SocketAddr,
        peer_policy: PinPolicy,
    ) -> Result<InteractiveConnection, NetError> {
        let cert = build_tls_certificate(identity)?;
        let client_config = build_client_config(&cert, peer_policy)?;
        let connecting = self
            .endpoint
            .connect_with(client_config, addr, "mouser")
            .map_err(|e| NetError::Connect(e.to_string()))?;
        let connection = connecting
            .await
            .map_err(|e| NetError::Connect(e.to_string()))?;
        InteractiveConnection::establish(connection, ConnectionEnd::Initiator).await
    }

    /// Accept the next inbound interactive connection (§6.1). The control stream is
    /// accepted and the priming frame consumed before returning (A2).
    pub async fn accept_interactive(&self) -> Result<InteractiveConnection, NetError> {
        let incoming = self
            .endpoint
            .accept()
            .await
            .ok_or_else(|| NetError::Connect("endpoint closed".to_string()))?;
        let connection = incoming
            .await
            .map_err(|e| NetError::Connect(e.to_string()))?;
        InteractiveConnection::establish(connection, ConnectionEnd::Acceptor).await
    }

    /// Close the endpoint and drain in-flight connections gracefully (H6): send each
    /// peer a CONNECTION_CLOSE, then wait for them to acknowledge / go idle.
    pub async fn shutdown(self) {
        self.endpoint.close(0u32.into(), b"shutdown");
        self.endpoint.wait_idle().await;
    }

    /// Close the endpoint immediately, terminating all connections without draining.
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
    let mut config = ServerConfig::with_crypto(Arc::new(quic));
    config.transport_config(transport_config()?);
    Ok(config)
}

fn build_client_config(
    cert: &TlsCertificate,
    peer_policy: PinPolicy,
) -> Result<ClientConfig, NetError> {
    let rustls_config = tls::client_config(cert, peer_policy)?;
    let quic =
        QuicClientConfig::try_from(rustls_config).map_err(|e| NetError::Tls(e.to_string()))?;
    let mut config = ClientConfig::new(Arc::new(quic));
    config.transport_config(transport_config()?);
    Ok(config)
}

/// An established interactive connection (§6.1): a long-lived bidi control stream plus
/// the QUIC datagram plane for pointer motion. The control stream is materialized
/// eagerly at construction (A2); motion is sent through a keep-newest pump (A4).
pub struct InteractiveConnection {
    connection: Connection,
    control: ControlStream,
    motion: MotionSender,
}

/// The long-lived bidi control stream. `recv` holds a persistent buffer so
/// [`InteractiveConnection::recv_control`] is cancel-safe (A3).
struct ControlStream {
    send: Mutex<SendStream>,
    recv: Mutex<RecvState>,
}

/// Receive-side state for the control stream: the quinn stream plus bytes read but not
/// yet consumed as a complete frame. Buffering here (not on the stack of a `recv`
/// future) is what makes [`InteractiveConnection::recv_control`] cancel-safe (A3): a
/// dropped recv future leaves already-read bytes intact in `buf`.
struct RecvState {
    stream: RecvStream,
    buf: Vec<u8>,
}

impl InteractiveConnection {
    /// Establish the control stream eagerly and symmetrically (A2), then start the
    /// keep-newest motion pump (A4).
    async fn establish(connection: Connection, end: ConnectionEnd) -> Result<Self, NetError> {
        let (send, recv) = match end {
            ConnectionEnd::Initiator => {
                // Open the bidi stream AND write a priming frame so the stream
                // materializes on the peer's `accept_bi` regardless of who sends first.
                let (mut send, recv) = connection
                    .open_bi()
                    .await
                    .map_err(|e| NetError::Connect(e.to_string()))?;
                let prime = mouser_protocol::encode_frame(TYPE_STREAM_PRIME, 0, &[])
                    .map_err(|e| NetError::Frame(e.to_string()))?;
                send.write_all(&prime)
                    .await
                    .map_err(|e| NetError::Io(e.to_string()))?;
                (send, recv)
            }
            ConnectionEnd::Acceptor => {
                let (send, mut recv) = connection
                    .accept_bi()
                    .await
                    .map_err(|e| NetError::Connect(e.to_string()))?;
                // Consume the initiator's priming frame. This runs once during setup
                // (never under a `select!`), so a blocking read is fine here.
                consume_prime(&mut recv).await?;
                (send, recv)
            }
        };
        let control = ControlStream {
            send: Mutex::new(send),
            recv: Mutex::new(RecvState {
                stream: recv,
                buf: Vec::new(),
            }),
        };
        let motion = MotionSender::spawn(connection.clone());
        Ok(Self {
            connection,
            control,
            motion,
        })
    }

    /// The negotiated ALPN (should be [`tls::ALPN_MOUSER_1`]) — proves §2 versioning.
    pub fn negotiated_alpn(&self) -> Option<Vec<u8>> {
        self.connection
            .handshake_data()
            .and_then(|d| d.downcast::<quinn::crypto::rustls::HandshakeData>().ok())
            .and_then(|d| d.protocol)
    }

    /// The peer's pinned `device_id` derived from its presented leaf cert (§3).
    pub fn peer_device_id(&self) -> Option<DeviceId> {
        let identity = self.connection.peer_identity()?;
        let certs = identity
            .downcast::<Vec<rustls_pki_types::CertificateDer<'static>>>()
            .ok()?;
        let leaf = certs.first()?;
        device_id_from_cert(leaf).ok()
    }

    /// Send a framed control message (§0.2): `encode_frame(msg_type, 0, payload)` on
    /// the control stream. `payload` is the CBOR body produced by `mouser-protocol`.
    pub async fn send_control(&self, msg_type: u16, payload: &[u8]) -> Result<(), NetError> {
        let frame = mouser_protocol::encode_frame(msg_type, 0, payload)
            .map_err(|e| NetError::Frame(e.to_string()))?;
        let mut send = self.control.send.lock().await;
        send.write_all(&frame)
            .await
            .map_err(|e| NetError::Io(e.to_string()))?;
        Ok(())
    }

    /// Receive one framed control message, returning `(msg_type, payload_bytes)` (§0.2).
    ///
    /// **Cancel-safe (A3):** bytes are accumulated in a persistent per-stream buffer and
    /// a frame is only removed once fully present, so dropping this future (e.g. under a
    /// `tokio::select!` / timeout) never corrupts the framed stream.
    pub async fn recv_control(&self) -> Result<(u16, Vec<u8>), NetError> {
        let mut recv = self.control.recv.lock().await;
        recv_frame(&mut recv).await
    }

    /// Queue a `PointerMotion` for unreliable delivery (§7.6 tag 0x01) through the
    /// keep-newest sender (A4). Non-blocking; the newest position wins.
    pub fn send_motion(&self, motion: &mouser_protocol::PointerMotion) -> Result<(), NetError> {
        let bytes = mouser_protocol::encode_motion(motion)
            .map_err(|e| NetError::Datagram(e.to_string()))?;
        self.motion.send(Bytes::from(bytes));
        Ok(())
    }

    /// Receive and decode the next motion datagram (§7.6).
    ///
    /// Drop-and-continue (H8): a corrupt or unknown datagram yields
    /// `Ok(Datagram::Unknown(..))` (the caller drops it); `Err` is reserved for the
    /// underlying QUIC read failure (a genuinely dead/closed connection).
    pub async fn recv_motion(&self) -> Result<mouser_protocol::Datagram, NetError> {
        let bytes = self
            .connection
            .read_datagram()
            .await
            .map_err(|e| NetError::Datagram(e.to_string()))?;
        match mouser_protocol::decode_datagram(&bytes) {
            Ok(datagram) => Ok(datagram),
            // A bad UDP packet is not a dead connection: drop it and keep going.
            Err(_) => Ok(mouser_protocol::Datagram::Unknown(
                bytes.first().copied().unwrap_or(0),
            )),
        }
    }

    /// Gracefully shut down the connection (H6): send the peer a CONNECTION_CLOSE and
    /// await the close handshake so it isn't left waiting for an idle timeout.
    pub async fn shutdown(&self) {
        self.connection.close(0u32.into(), b"bye");
        self.connection.closed().await;
    }

    /// Close the connection immediately without awaiting the drain.
    pub fn close(&self) {
        self.connection.close(0u32.into(), b"bye");
    }
}

impl Drop for InteractiveConnection {
    fn drop(&mut self) {
        // Best-effort graceful close (H6): tell the peer we're gone so it doesn't have
        // to wait out the idle timeout. quinn flushes the CONNECTION_CLOSE on drop.
        self.connection.close(0u32.into(), b"bye");
    }
}

/// Read and discard exactly one priming frame from a freshly accepted control stream
/// (A2). Setup-only and not cancellation-exposed, so `read_exact` is safe here.
async fn consume_prime(recv: &mut RecvStream) -> Result<(), NetError> {
    let mut header = [0u8; 8];
    recv.read_exact(&mut header)
        .await
        .map_err(|e| NetError::Io(e.to_string()))?;
    let (_msg_type, payload_len) = parse_frame_header(&header)?;
    if payload_len > 0 {
        let mut payload = vec![0u8; payload_len];
        recv.read_exact(&mut payload)
            .await
            .map_err(|e| NetError::Io(e.to_string()))?;
    }
    Ok(())
}

/// Cancel-safe framed read (A3). Pulls bytes into `state.buf` with individually
/// cancel-safe `read()` calls and only removes a frame once it is fully buffered, so a
/// dropped future loses nothing.
async fn recv_frame(state: &mut RecvState) -> Result<(u16, Vec<u8>), NetError> {
    // Ensure the 8-byte header is buffered.
    fill_to(state, 8).await?;
    let header: [u8; 8] = state
        .buf
        .get(0..8)
        .and_then(|b| b.try_into().ok())
        .ok_or_else(|| NetError::Frame("short control frame header".to_string()))?;
    let (msg_type, payload_len) = parse_frame_header(&header)?;

    // Ensure header + payload are buffered, then split off exactly one frame.
    let frame_len = 8 + payload_len;
    fill_to(state, frame_len).await?;
    let mut frame: Vec<u8> = state.buf.drain(0..frame_len).collect();
    let payload = frame.split_off(8);
    Ok((msg_type, payload))
}

/// Read from the stream into `state.buf` until it holds at least `needed` bytes. Each
/// `read()` is cancel-safe; partial progress is preserved in `state.buf` (A3).
async fn fill_to(state: &mut RecvState, needed: usize) -> Result<(), NetError> {
    let mut chunk = [0u8; 4096];
    while state.buf.len() < needed {
        match state
            .stream
            .read(&mut chunk)
            .await
            .map_err(|e| NetError::Io(e.to_string()))?
        {
            Some(0) | None => {
                return Err(NetError::Io("control stream closed".to_string()));
            }
            Some(n) => {
                let read = chunk.get(..n).ok_or_else(|| {
                    NetError::Io("control stream read overran buffer".to_string())
                })?;
                state.buf.extend_from_slice(read);
            }
        }
    }
    Ok(())
}

/// Parse the §0.2 frame header, returning `(msg_type, payload_len)`. Checked slicing
/// (no panicking index — §0.3): `len: u32 (LE) | type: u16 (LE) | flags: u16 (LE)`.
fn parse_frame_header(header: &[u8; 8]) -> Result<(u16, usize), NetError> {
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
    // `len` counts the type+flags+payload (header bytes after the length); payload is
    // that minus the 4-byte type+flags.
    let payload_len = (len - 4) as usize;
    Ok((msg_type, payload_len))
}
