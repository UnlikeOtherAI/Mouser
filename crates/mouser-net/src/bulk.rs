//! The **bulk connection** (§6.2): a *second* QUIC connection per peer, separate from
//! the interactive one, carrying file transfers / large clipboard payloads / state
//! snapshots so bulk data never fills the interactive congestion window.
//!
//! It reuses the **exact** cert / pin / TLS-1.3 / ALPN builders of the interactive
//! plane ([`crate::transport`]) — one security root, no divergent copy. On top of that
//! it adds the §5 step-5 binding:
//!
//! - The dialer opens a dedicated **control** bidi stream and sends [`BulkHello`]
//!   carrying its `device_id`, the `interactive_session_id` it binds to, and
//!   `channel_sig = Ed25519_sign(identity_key, tls_exporter(label="mouser-bulk-v1",
//!   context = device_id || be8(interactive_session_id), length = 64))` over **this**
//!   (bulk) connection's exporter.
//! - The acceptor verifies the binding against the **same** Ed25519 key it pinned (the
//!   leaf cert's key) and the `interactive_session_id` it expects, so the two
//!   connections are cryptographically tied and a relayed/replayed bulk plane is
//!   rejected. No second SAS (trust is already established on the interactive plane).
//!
//! Each transfer then gets **one dedicated bidi stream per `transfer_id`** (§6.2),
//! carrying §0.2-framed `FileOffer`/`FileChunk`/… via [`TransferStream`].

use std::net::SocketAddr;

use ed25519_dalek::{Signature, Verifier};
use quinn::{Connection, Endpoint, RecvStream, SendStream};

use crate::identity::{build_tls_certificate, device_id_from_cert, verifying_key_from_cert};
use crate::pin::PinPolicy;
use crate::transport::{build_client_config, build_server_config};
use crate::NetError;
use mouser_core::DeviceIdentity;

/// TLS exporter label for the bulk binding proof (§5 step 5).
const BULK_LABEL: &[u8] = b"mouser-bulk-v1";
/// Length of the exporter output that gets signed (§5 step 5).
const BULK_EXPORT_LEN: usize = 64;

/// A bound QUIC endpoint for the bulk plane. Mirrors [`crate::transport::InteractiveEndpoint`]
/// but every accepted/dialed connection completes the [`BulkHello`] binding handshake.
pub struct BulkEndpoint {
    endpoint: Endpoint,
}

impl BulkEndpoint {
    /// Bind a bulk **server** endpoint presenting `identity`'s cert and pinning the
    /// dialing peer per `peer_policy` (§3) — the same builders as the interactive plane.
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

    /// Bind a client-only bulk endpoint (no listener) for dialing peers.
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

    /// Dial `addr`, pin the server per `peer_policy`, then send the [`BulkHello`] that
    /// binds this bulk connection to `interactive_session_id` (§5 step 5).
    pub async fn connect_bulk(
        &self,
        identity: &DeviceIdentity,
        addr: SocketAddr,
        peer_policy: PinPolicy,
        interactive_session_id: u64,
    ) -> Result<BulkConnection, NetError> {
        let cert = build_tls_certificate(identity)?;
        let client_config = build_client_config(&cert, peer_policy)?;
        let connection = self
            .endpoint
            .connect_with(client_config, addr, "mouser")
            .map_err(|e| NetError::Connect(e.to_string()))?
            .await
            .map_err(|e| NetError::Connect(e.to_string()))?;

        // Open the control stream and send BulkHello. The exporter is bound to this
        // (bulk) connection, and the signed context ties in the interactive session.
        let (mut send, _recv) = connection
            .open_bi()
            .await
            .map_err(|e| NetError::Connect(e.to_string()))?;
        let hello = build_bulk_hello(identity, &connection, interactive_session_id)?;
        let payload =
            mouser_protocol::to_cbor(&hello).map_err(|e| NetError::Frame(e.to_string()))?;
        let frame = mouser_protocol::encode_frame(mouser_protocol::TYPE_BULK_HELLO, 0, &payload)
            .map_err(|e| NetError::Frame(e.to_string()))?;
        send.write_all(&frame)
            .await
            .map_err(|e| NetError::Io(e.to_string()))?;
        // Keep the control stream's send half resident so the acceptor's `accept_bi`
        // resolves and the binding stream isn't torn down mid-handshake.
        Ok(BulkConnection {
            connection,
            _control_send: send,
        })
    }

    /// Accept the next inbound bulk connection and verify its [`BulkHello`] binds to
    /// `expected_session_id` (§5 step 5). Rejects a binding for a different session.
    pub async fn accept_bulk(&self, expected_session_id: u64) -> Result<BulkConnection, NetError> {
        let connection = self
            .endpoint
            .accept()
            .await
            .ok_or_else(|| NetError::Connect("endpoint closed".to_string()))?
            .await
            .map_err(|e| NetError::Connect(e.to_string()))?;

        let (_send, mut recv) = connection
            .accept_bi()
            .await
            .map_err(|e| NetError::Connect(e.to_string()))?;
        let (msg_type, payload) = read_frame(&mut recv, mouser_protocol::MAX_CONTROL_FRAME).await?;
        if msg_type != mouser_protocol::TYPE_BULK_HELLO {
            return Err(NetError::Connect(format!(
                "expected BulkHello (0x04), got type {msg_type:#06x}"
            )));
        }
        let hello: mouser_protocol::BulkHello =
            mouser_protocol::from_cbor(&payload).map_err(|e| NetError::Frame(e.to_string()))?;
        verify_bulk_hello(&hello, &connection, expected_session_id)?;
        Ok(BulkConnection {
            connection,
            _control_send: _send,
        })
    }

    /// Gracefully shut down the bulk endpoint (H6, mirrored from the interactive plane):
    /// send each peer a CONNECTION_CLOSE then wait for in-flight connections to drain, so
    /// a peer mid-transfer isn't left waiting out the idle timeout.
    pub async fn shutdown(self) {
        self.endpoint.close(0u32.into(), b"shutdown");
        self.endpoint.wait_idle().await;
    }

    /// Close the endpoint immediately, terminating all bulk connections without draining.
    pub fn close(&self) {
        self.endpoint.close(0u32.into(), b"shutdown");
    }
}

/// The exporter→signature input for the bulk binding (§5 step 5): `tls_exporter(label,
/// context = device_id || be8(interactive_session_id), length = 64)` over `connection`.
fn bulk_exporter(
    connection: &Connection,
    device_id: &[u8; 32],
    interactive_session_id: u64,
) -> Result<[u8; BULK_EXPORT_LEN], NetError> {
    let mut context = Vec::with_capacity(40);
    context.extend_from_slice(device_id);
    context.extend_from_slice(&interactive_session_id.to_be_bytes());
    let mut out = [0u8; BULK_EXPORT_LEN];
    connection
        .export_keying_material(&mut out, BULK_LABEL, &context)
        .map_err(|_| NetError::Tls("bulk exporter unavailable".to_string()))?;
    Ok(out)
}

fn build_bulk_hello(
    identity: &DeviceIdentity,
    connection: &Connection,
    interactive_session_id: u64,
) -> Result<mouser_protocol::BulkHello, NetError> {
    let device_id = identity.device_id();
    let to_sign = bulk_exporter(connection, &device_id, interactive_session_id)?;
    let signature: Signature = identity.sign(&to_sign);
    Ok(mouser_protocol::BulkHello {
        device_id: device_id.to_vec(),
        interactive_session_id,
        channel_sig: signature.to_vec(),
    })
}

/// Verify a received [`BulkHello`]: the claimed `device_id` matches the pinned cert,
/// the session id is the one we expect, and `channel_sig` verifies against the leaf
/// cert's Ed25519 key over the bulk exporter (§5 step 5).
fn verify_bulk_hello(
    hello: &mouser_protocol::BulkHello,
    connection: &Connection,
    expected_session_id: u64,
) -> Result<(), NetError> {
    if hello.interactive_session_id != expected_session_id {
        return Err(NetError::Connect(
            "BulkHello binds to a different interactive session".to_string(),
        ));
    }
    let leaf = peer_leaf_cert(connection)?;
    let pinned_id = device_id_from_cert(&leaf)?;
    let claimed: [u8; 32] = hello
        .device_id
        .as_slice()
        .try_into()
        .map_err(|_| NetError::Connect("BulkHello device_id is not 32 bytes".to_string()))?;
    if claimed != pinned_id {
        return Err(NetError::Connect(
            "BulkHello device_id does not match the pinned cert".to_string(),
        ));
    }
    let verifying = verifying_key_from_cert(&leaf)?;
    let expected = bulk_exporter(connection, &pinned_id, expected_session_id)?;
    let signature = Signature::from_slice(&hello.channel_sig)
        .map_err(|e| NetError::Connect(format!("malformed channel_sig: {e}")))?;
    verifying
        .verify(&expected, &signature)
        .map_err(|_| NetError::Connect("bulk channel_sig verification failed".to_string()))?;
    Ok(())
}

fn peer_leaf_cert(
    connection: &Connection,
) -> Result<rustls_pki_types::CertificateDer<'static>, NetError> {
    let identity = connection
        .peer_identity()
        .ok_or_else(|| NetError::Connect("peer presented no certificate".to_string()))?;
    let certs = identity
        .downcast::<Vec<rustls_pki_types::CertificateDer<'static>>>()
        .map_err(|_| NetError::Connect("unexpected peer identity type".to_string()))?;
    certs
        .first()
        .cloned()
        .ok_or_else(|| NetError::Connect("empty peer certificate chain".to_string()))
}

/// An established, identity-bound bulk connection (§6.2). Spawns one bidi stream per
/// `transfer_id`; the §0.2-framed transfer messages ride that stream.
pub struct BulkConnection {
    connection: Connection,
    // The binding control stream's send half is kept alive so the stream persists for
    // the connection's lifetime (closing it would let the peer observe a reset).
    _control_send: SendStream,
}

impl BulkConnection {
    /// The peer's pinned `device_id` from its presented leaf cert (§3).
    pub fn peer_device_id(&self) -> Option<[u8; 32]> {
        peer_leaf_cert(&self.connection)
            .ok()
            .and_then(|c| device_id_from_cert(&c).ok())
    }

    /// Open a fresh dedicated bidi stream for `transfer_id` (§6.2). The first frame the
    /// caller writes (the `FileOffer`) materializes the stream on the peer.
    pub async fn open_transfer(&self, _transfer_id: u64) -> Result<TransferStream, NetError> {
        let (send, recv) = self
            .connection
            .open_bi()
            .await
            .map_err(|e| NetError::Connect(e.to_string()))?;
        Ok(TransferStream { send, recv })
    }

    /// Accept the next transfer's dedicated bidi stream (§6.2).
    pub async fn accept_transfer(&self) -> Result<TransferStream, NetError> {
        let (send, recv) = self
            .connection
            .accept_bi()
            .await
            .map_err(|e| NetError::Connect(e.to_string()))?;
        Ok(TransferStream { send, recv })
    }

    /// Gracefully shut down the bulk connection (H6, mirrored from the interactive plane):
    /// send the peer a CONNECTION_CLOSE and await the close handshake, so it isn't left
    /// waiting for an idle timeout after a transfer completes.
    pub async fn shutdown(&self) {
        self.connection.close(0u32.into(), b"bye");
        self.connection.closed().await;
    }

    /// Close the bulk connection immediately without awaiting the drain.
    pub fn close(&self) {
        self.connection.close(0u32.into(), b"bye");
    }
}

impl Drop for BulkConnection {
    fn drop(&mut self) {
        // Best-effort graceful close (H6): tell the peer we're gone so it doesn't have to
        // wait out the idle timeout. quinn flushes the CONNECTION_CLOSE on drop.
        self.connection.close(0u32.into(), b"bye");
    }
}

/// One transfer's dedicated bidi stream (§6.2): sends/receives §0.2-framed messages
/// (the CBOR payloads from `mouser-protocol`). The caller owns the `mouser-files`
/// state machine; this type is just the framed pipe.
pub struct TransferStream {
    send: SendStream,
    recv: RecvStream,
}

impl TransferStream {
    /// Send one §0.2-framed bulk message: `encode_bulk_frame(msg_type, 0, payload)`.
    /// Uses the bulk size ceiling so a 1 MiB `FileChunk` fits once CBOR-wrapped (§0.3).
    pub async fn send_msg(&mut self, msg_type: u16, payload: &[u8]) -> Result<(), NetError> {
        let frame = mouser_protocol::encode_bulk_frame(msg_type, 0, payload)
            .map_err(|e| NetError::Frame(e.to_string()))?;
        self.send
            .write_all(&frame)
            .await
            .map_err(|e| NetError::Io(e.to_string()))?;
        Ok(())
    }

    /// Receive one §0.2-framed bulk message, returning `(msg_type, payload_bytes)`.
    pub async fn recv_msg(&mut self) -> Result<(u16, Vec<u8>), NetError> {
        read_frame(&mut self.recv, mouser_protocol::MAX_BULK_FRAME).await
    }

    /// Signal end-of-transfer by finishing the send half (peer sees clean EOF).
    pub fn finish(&mut self) -> Result<(), NetError> {
        self.send.finish().map_err(|e| NetError::Io(e.to_string()))
    }
}

/// Read one §0.2 frame (`len|type|flags|payload`) from a recv stream, rejecting any
/// `len` outside `4..=max` **before** allocating the payload (§0.3). Panic-free.
async fn read_frame(recv: &mut RecvStream, max: u32) -> Result<(u16, Vec<u8>), NetError> {
    let mut header = [0u8; 8];
    recv.read_exact(&mut header)
        .await
        .map_err(|e| NetError::Io(e.to_string()))?;
    let len_bytes: [u8; 4] = header
        .get(0..4)
        .and_then(|b| b.try_into().ok())
        .ok_or_else(|| NetError::Frame("short frame header".to_string()))?;
    let type_bytes: [u8; 2] = header
        .get(4..6)
        .and_then(|b| b.try_into().ok())
        .ok_or_else(|| NetError::Frame("short frame header".to_string()))?;
    let len = u32::from_le_bytes(len_bytes);
    let msg_type = u16::from_le_bytes(type_bytes);
    if !(4..=max).contains(&len) {
        return Err(NetError::Frame("frame length out of range".to_string()));
    }
    let payload_len = (len - 4) as usize;
    let mut payload = vec![0u8; payload_len];
    recv.read_exact(&mut payload)
        .await
        .map_err(|e| NetError::Io(e.to_string()))?;
    Ok((msg_type, payload))
}
