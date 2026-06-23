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
    ClientConfig, Connection, Endpoint, IdleTimeout, Incoming, ServerConfig, TransportConfig,
};

use crate::control::{self, ControlStream};
use crate::identity::{build_tls_certificate, device_id_from_cert, TlsCertificate};
use crate::motion::{MotionPlane, MotionSender};
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

/// Bound the quinn datagram *receive* backlog (C2-7 / audit-R2 transport MEDIUM). The
/// RX buffer is drained oldest-first and unbounded by default; under a burst a stale
/// `PointerMotion` could sit ahead of the newest one. A small bound keeps the receiver
/// converging on recent samples (newest-wins, §7.6) and caps inbound memory. 16 KiB is
/// a few dozen motion datagrams of backlog.
const DATAGRAM_RECV_BUFFER: usize = 16 * 1024;

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

/// Wildcard client-bind address matching the destination's family (port 0 →
/// OS-assigned). A client that dials a LAN/remote peer must bind the *unspecified*
/// address so the OS routes egress out the right interface — a loopback bind
/// (`127.0.0.1`) can only reach the same host and silently fails to a remote peer.
pub fn client_bind_for(dest: SocketAddr) -> SocketAddr {
    match dest {
        SocketAddr::V4(_) => SocketAddr::from(([0, 0, 0, 0], 0)),
        SocketAddr::V6(_) => SocketAddr::from(([0u16, 0, 0, 0, 0, 0, 0, 0], 0)),
    }
}

/// Unspecified IPv6 bind (`[::]:0`) for a dual-stack server listener — one socket
/// serving both IPv4 (v4-mapped) and IPv6 dialers (macOS/Linux default
/// `IPV6_V6ONLY=off`). Used by the daemon so a peer that resolves us to either family
/// can connect. See [`InteractiveEndpoint::bind_server`].
pub fn dual_stack_addr() -> SocketAddr {
    SocketAddr::from(([0u16, 0, 0, 0, 0, 0, 0, 0], 0))
}

/// Build the shared [`TransportConfig`] applied to both endpoints (H1, A4).
fn transport_config() -> Result<Arc<TransportConfig>, NetError> {
    let mut cfg = TransportConfig::default();
    let idle = IdleTimeout::try_from(MAX_IDLE).map_err(|e| NetError::Tls(e.to_string()))?;
    cfg.max_idle_timeout(Some(idle));
    cfg.keep_alive_interval(Some(KEEP_ALIVE));
    cfg.datagram_send_buffer_size(DATAGRAM_SEND_BUFFER);
    cfg.datagram_receive_buffer_size(Some(DATAGRAM_RECV_BUFFER));
    Ok(Arc::new(cfg))
}

/// A bound QUIC endpoint that can accept inbound interactive connections and dial out.
pub struct InteractiveEndpoint {
    endpoint: Endpoint,
    /// This endpoint's own `device_id` (§3), set when bound as a server so an accepted
    /// connection knows its local id for the §5 SAS derivation. A client-only endpoint
    /// dials with an explicit `identity`, so it carries the local id per-connect instead.
    my_device_id: Option<DeviceId>,
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
        // Bind via quinn so a `[::]` address listens **dual-stack** (one socket serving
        // both IPv4 v4-mapped and IPv6 dialers) — a LAN peer may resolve us to either
        // family, and an iPhone in particular dials us over IPv6. macOS/Linux default
        // IPV6_V6ONLY to off, so this is genuinely dual-stack there. (NB: hand-rolling
        // the socket with socket2 + `Endpoint::new` was tried and broke the QUIC
        // handshake, so we use quinn's own socket setup.)
        let endpoint =
            Endpoint::server(server_config, addr).map_err(|e| NetError::Io(e.to_string()))?;
        Ok(Self {
            endpoint,
            my_device_id: Some(identity.device_id()),
        })
    }

    /// Bind a client-only endpoint (no listener) for dialing peers.
    pub fn bind_client(addr: SocketAddr) -> Result<Self, NetError> {
        let endpoint = Endpoint::client(addr).map_err(|e| NetError::Io(e.to_string()))?;
        Ok(Self {
            endpoint,
            my_device_id: None,
        })
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
        InteractiveConnection::establish(connection, ConnectionEnd::Initiator, identity.device_id())
            .await
    }

    /// Accept the next inbound interactive connection (§6.1). The control stream is
    /// accepted and the priming frame consumed before returning (A2).
    pub async fn accept_interactive(&self) -> Result<InteractiveConnection, NetError> {
        let connection = accept_after_retry(&self.endpoint).await?;
        // A server endpoint always carries its own `device_id` (set at `bind_server`); a
        // client-only endpoint cannot accept, so this is `Some` on every real accept path.
        let my_device_id = self
            .my_device_id
            .ok_or_else(|| NetError::Connect("accept on a client-only endpoint".to_string()))?;
        InteractiveConnection::establish(connection, ConnectionEnd::Acceptor, my_device_id).await
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

async fn accept_after_retry(endpoint: &Endpoint) -> Result<Connection, NetError> {
    loop {
        let incoming = endpoint
            .accept()
            .await
            .ok_or_else(|| NetError::Connect("endpoint closed".to_string()))?;
        let incoming = retry_unvalidated(incoming);
        let Some(incoming) = incoming else {
            continue;
        };
        return incoming.await.map_err(|e| NetError::Connect(e.to_string()));
    }
}

fn retry_unvalidated(incoming: Incoming) -> Option<Incoming> {
    if incoming.remote_address_validated() {
        return Some(incoming);
    }
    match incoming.retry() {
        Ok(()) => None,
        Err(e) => Some(e.into_incoming()),
    }
}

// Exposed `pub(crate)` (additive) so the bulk connection (§6.2, `crate::bulk`) reuses
// the exact same cert/pin/TLS-1.3/ALPN builders as the interactive plane — there is one
// security root, never a divergent copy.
pub(crate) fn build_server_config(
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

pub(crate) fn build_client_config(
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
    /// This end's own `device_id` (§3), captured at establish so [`Self::sas`] can build
    /// the §5 ascending-id exporter context without re-deriving from the local cert.
    my_device_id: DeviceId,
}

impl InteractiveConnection {
    /// Establish the control stream eagerly and symmetrically (A2), then start the
    /// keep-newest motion pump (A4). `my_device_id` is this end's own id (§3), retained
    /// for the §5 SAS derivation.
    async fn establish(
        connection: Connection,
        end: ConnectionEnd,
        my_device_id: DeviceId,
    ) -> Result<Self, NetError> {
        // `seed` carries any bytes already read off the recv stream during setup that are
        // NOT the priming frame, so the first real frame isn't lost (consume_prime
        // hardening): they pre-fill the persistent recv buffer.
        let (send, recv, seed) = match end {
            ConnectionEnd::Initiator => {
                // Open the bidi stream AND write a priming frame so the stream
                // materializes on the peer's `accept_bi` regardless of who sends first.
                let (mut send, recv) = connection
                    .open_bi()
                    .await
                    .map_err(|e| NetError::Connect(e.to_string()))?;
                let prime = mouser_protocol::encode_frame(control::TYPE_STREAM_PRIME, 0, &[])
                    .map_err(|e| NetError::Frame(e.to_string()))?;
                send.write_all(&prime)
                    .await
                    .map_err(|e| NetError::Io(e.to_string()))?;
                (send, recv, Vec::new())
            }
            ConnectionEnd::Acceptor => {
                let (send, mut recv) = connection
                    .accept_bi()
                    .await
                    .map_err(|e| NetError::Connect(e.to_string()))?;
                // Consume the initiator's priming frame — but only discard it if it really
                // is a `TYPE_STREAM_PRIME`. If a peer materialized the stream with a real
                // first frame instead, keep its bytes so `recv_control` returns it rather
                // than blind-discarding a real message. Setup-only (never under a
                // `select!`), so the blocking reads here are fine.
                let seed = control::consume_prime(&mut recv).await?;
                (send, recv, seed)
            }
        };
        let control = ControlStream::new(send, recv, seed);
        let motion = MotionSender::spawn(connection.clone());
        Ok(Self {
            connection,
            control,
            motion,
            my_device_id,
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

    /// The mandatory 6-digit §5 SAS for this interactive connection, derived from the
    /// TLS 1.3 exporter and the two `device_id`s ordered ascending. Both ends compute the
    /// **identical** string; the user compares the two screens to authenticate the channel
    /// (§5 step 3). Errors if the peer presented no usable cert or the exporter is
    /// unavailable. See [`crate::sas`] for the full derivation.
    pub fn sas(&self) -> Result<String, NetError> {
        let peer_id = self
            .peer_device_id()
            .ok_or_else(|| NetError::Connect("no peer device_id for SAS".to_string()))?;
        crate::sas::compute_sas(&self.connection, self.my_device_id, peer_id)
    }

    /// Send a framed control message (§0.2): `encode_frame(msg_type, 0, payload)` on
    /// the control stream. `payload` is the CBOR body produced by `mouser-protocol`.
    ///
    /// **Cancel-safe:** the encoded frame is handed to a dedicated writer task that owns
    /// the [`SendStream`] and writes one whole frame at a time; this method only enqueues
    /// the frame and then awaits a oneshot acking that it is fully on the wire. Dropping
    /// this future (e.g. under a `tokio::select!` / timeout) cannot leave a partial frame:
    /// the frame is already queued and the writer still flushes it completely, so the next
    /// frame is never corrupted.
    pub async fn send_control(&self, msg_type: u16, payload: &[u8]) -> Result<(), NetError> {
        self.control.send(msg_type, payload).await
    }

    /// The current pointer-motion transport (C2-7 / §6.1). When this reads
    /// [`MotionPlane::ControlFallback`] the datagram plane is unavailable for this
    /// connection and the engine must route `PointerMotion` over the control stream.
    pub fn motion_plane(&self) -> tokio::sync::watch::Receiver<MotionPlane> {
        self.motion.plane()
    }

    /// Receive one framed control message, returning `(msg_type, payload_bytes)` (§0.2).
    ///
    /// **Cancel-safe (A3):** bytes are accumulated in a persistent per-stream buffer and
    /// a frame is only removed once fully present, so dropping this future (e.g. under a
    /// `tokio::select!` / timeout) never corrupts the framed stream.
    pub async fn recv_control(&self) -> Result<(u16, Vec<u8>), NetError> {
        self.control.recv().await
    }

    /// Queue a `PointerMotion` for unreliable delivery (§7.6 tag 0x01) through the
    /// keep-newest sender (A4). Non-blocking; the newest position wins.
    pub fn send_motion(&self, motion: &mouser_protocol::PointerMotion) -> Result<(), NetError> {
        let bytes = mouser_protocol::encode_motion(motion)
            .map_err(|e| NetError::Datagram(e.to_string()))?;
        self.motion.send(Bytes::from(bytes));
        Ok(())
    }

    /// Receive the next *valid* motion datagram (§7.6).
    ///
    /// Drop-and-continue (H8): a corrupt datagram or an unknown tag is dropped and the
    /// next datagram is read, so a single bad UDP packet never surfaces to the caller
    /// and never tears down the connection. `Err` is reserved for the underlying QUIC
    /// read failure (a genuinely dead/closed connection).
    pub async fn recv_motion(&self) -> Result<mouser_protocol::Datagram, NetError> {
        loop {
            let bytes = self
                .connection
                .read_datagram()
                .await
                .map_err(|e| NetError::Datagram(e.to_string()))?;
            match mouser_protocol::decode_datagram(&bytes) {
                // Unknown tag or undecodable body: a bad packet, not a dead connection.
                Ok(mouser_protocol::Datagram::Unknown(_)) | Err(_) => continue,
                Ok(datagram) => return Ok(datagram),
            }
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
        // Stop the control writer task (its mpsc sender is dropped with `self.control`,
        // but abort makes teardown immediate and frees the held SendStream).
        self.control.abort_writer();
        // Best-effort graceful close (H6): tell the peer we're gone so it doesn't have
        // to wait out the idle timeout. quinn flushes the CONNECTION_CLOSE on drop.
        self.connection.close(0u32.into(), b"bye");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Regression: a client that dials a LAN/remote peer must bind the *unspecified*
    // address, never loopback — a `127.0.0.1`-bound UDP socket cannot transmit to a
    // remote host, so the mobile client could never reach a desktop over wifi.
    #[test]
    fn client_bind_is_routable_not_loopback() {
        let v4_peer: SocketAddr = "192.168.1.229:61600".parse().unwrap();
        let bind = client_bind_for(v4_peer);
        assert!(bind.ip().is_unspecified(), "{bind} must be unspecified");
        assert!(!bind.ip().is_loopback(), "{bind} must not be loopback");
        assert!(bind.is_ipv4(), "IPv4 peer -> IPv4 bind");
        assert_eq!(bind.port(), 0, "OS-assigned ephemeral port");

        let v6_peer: SocketAddr = "[fe80::1]:61600".parse().unwrap();
        let bind6 = client_bind_for(v6_peer);
        assert!(bind6.ip().is_unspecified());
        assert!(bind6.is_ipv6(), "IPv6 peer -> IPv6 bind");
    }
}
