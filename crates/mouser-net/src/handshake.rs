//! Interactive §5 `Hello` / `HelloAck` channel-binding handshake.
//!
//! The raw QUIC/TLS connection and its control stream are not exposed until this
//! module has verified the peer's `channel_sig` against the leaf cert key pinned by
//! rustls. The daemon may still require a human SAS approval for first contact, but
//! every returned interactive connection is already channel-verified.

use std::collections::BTreeSet;

use ed25519_dalek::{Signature, Verifier};
use mouser_core::{DeviceId, DeviceIdentity};
use mouser_protocol::{
    AckStatus, Capability, CapabilitySet, Hello, HelloAck, Os, Role, TYPE_HELLO, TYPE_HELLO_ACK,
};
use quinn::Connection;
use rand_core::{OsRng, RngCore};

use crate::control::ControlStream;
use crate::identity::{device_id_from_cert, peer_leaf_cert, verifying_key_from_cert};
use crate::NetError;

/// TLS exporter label for the interactive channel proof (§5 step 4).
const CHANNEL_LABEL: &[u8] = b"mouser-channel-v1";
/// Length of the exporter output that gets signed (§5 step 4).
const CHANNEL_EXPORT_LEN: usize = 64;

#[derive(Debug)]
pub(crate) struct VerifiedHello {
    pub(crate) peer_id: DeviceId,
    pub(crate) peer_name: Option<String>,
    pub(crate) local_session_id: u64,
    pub(crate) peer_session_id: u64,
}

/// Exchange and verify §7.1 `Hello`, then exchange `HelloAck`.
pub(crate) async fn exchange(
    connection: &Connection,
    control: &ControlStream,
    identity: &DeviceIdentity,
) -> Result<VerifiedHello, NetError> {
    let local_session_id = OsRng.next_u64();
    let local = build_hello(identity, connection, local_session_id)?;
    send_hello(control, &local).await?;

    let peer = match recv_hello(control).await {
        Ok(peer) => peer,
        Err(e) => {
            let _ = send_ack(control, AckStatus::Rejected, Some(e.to_string())).await;
            return Err(e);
        }
    };
    let peer_id = match verify_hello(&peer, connection) {
        Ok(peer_id) => peer_id,
        Err(e) => {
            let _ = send_ack(control, AckStatus::Rejected, Some(e.to_string())).await;
            return Err(e);
        }
    };
    send_ack(control, AckStatus::Accepted, None).await?;

    let ack = recv_ack(control).await?;
    if ack.status != AckStatus::Accepted {
        return Err(NetError::Connect(
            ack.reason
                .unwrap_or_else(|| "peer rejected Hello".to_string()),
        ));
    }

    Ok(VerifiedHello {
        peer_id,
        peer_name: non_empty(peer.name),
        local_session_id,
        peer_session_id: peer.session_id,
    })
}

fn build_hello(
    identity: &DeviceIdentity,
    connection: &Connection,
    session_id: u64,
) -> Result<Hello, NetError> {
    let device_id = identity.device_id();
    let to_sign = channel_exporter(connection, &device_id)?;
    let signature: Signature = identity.sign(&to_sign);
    Ok(Hello {
        device_id: device_id.to_vec(),
        name: String::new(),
        os: local_os(),
        engine_version: env!("CARGO_PKG_VERSION").to_string(),
        capabilities: local_capabilities(),
        role: Role::Eligible,
        session_id,
        channel_sig: signature.to_vec(),
    })
}

fn local_capabilities() -> CapabilitySet {
    CapabilitySet(BTreeSet::from([
        Capability::Keyboard,
        Capability::Mouse,
        Capability::Clipboard,
        Capability::FileTransfer,
    ]))
}

fn local_os() -> Os {
    if cfg!(target_os = "macos") {
        Os::Macos
    } else if cfg!(target_os = "windows") {
        Os::Windows
    } else if cfg!(target_os = "linux") {
        Os::Linux
    } else if cfg!(target_os = "ios") {
        Os::Ios
    } else if cfg!(target_os = "android") {
        Os::Android
    } else {
        Os::Unknown
    }
}

fn channel_exporter(
    connection: &Connection,
    device_id: &DeviceId,
) -> Result<[u8; CHANNEL_EXPORT_LEN], NetError> {
    let mut out = [0u8; CHANNEL_EXPORT_LEN];
    connection
        .export_keying_material(&mut out, CHANNEL_LABEL, device_id)
        .map_err(|_| NetError::Tls("interactive channel exporter unavailable".to_string()))?;
    Ok(out)
}

async fn send_hello(control: &ControlStream, hello: &Hello) -> Result<(), NetError> {
    let payload = mouser_protocol::to_cbor(hello).map_err(|e| NetError::Frame(e.to_string()))?;
    control.send(TYPE_HELLO, &payload).await
}

async fn recv_hello(control: &ControlStream) -> Result<Hello, NetError> {
    let (ty, payload) = control.recv().await?;
    if ty != TYPE_HELLO {
        return Err(NetError::Connect(format!(
            "expected Hello (0x01), got type {ty:#06x}"
        )));
    }
    mouser_protocol::from_cbor(&payload).map_err(|e| NetError::Frame(e.to_string()))
}

async fn send_ack(
    control: &ControlStream,
    status: AckStatus,
    reason: Option<String>,
) -> Result<(), NetError> {
    let ack = HelloAck { status, reason };
    let payload = mouser_protocol::to_cbor(&ack).map_err(|e| NetError::Frame(e.to_string()))?;
    control.send(TYPE_HELLO_ACK, &payload).await
}

async fn recv_ack(control: &ControlStream) -> Result<HelloAck, NetError> {
    let (ty, payload) = control.recv().await?;
    if ty != TYPE_HELLO_ACK {
        return Err(NetError::Connect(format!(
            "expected HelloAck (0x02), got type {ty:#06x}"
        )));
    }
    mouser_protocol::from_cbor(&payload).map_err(|e| NetError::Frame(e.to_string()))
}

fn verify_hello(hello: &Hello, connection: &Connection) -> Result<DeviceId, NetError> {
    let leaf = peer_leaf_cert(connection)?;
    let pinned_id = device_id_from_cert(&leaf)?;
    let claimed: DeviceId = hello
        .device_id
        .as_slice()
        .try_into()
        .map_err(|_| NetError::Connect("Hello device_id is not 32 bytes".to_string()))?;
    if claimed != pinned_id {
        return Err(NetError::Connect(
            "Hello device_id does not match the pinned cert".to_string(),
        ));
    }
    let verifying = verifying_key_from_cert(&leaf)?;
    let expected = channel_exporter(connection, &pinned_id)?;
    let signature = Signature::from_slice(&hello.channel_sig)
        .map_err(|e| NetError::Connect(format!("malformed channel_sig: {e}")))?;
    verifying.verify(&expected, &signature).map_err(|_| {
        NetError::Connect("interactive channel_sig verification failed".to_string())
    })?;
    Ok(pinned_id)
}

fn non_empty(value: String) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::control::{self, ControlStream};
    use crate::identity::build_tls_certificate;
    use crate::pin::PinPolicy;
    use crate::transport::{build_client_config, build_server_config};
    use mouser_protocol::{
        to_cbor, ClipboardOffer, FileEntry, FileOffer, KeyEvent, TYPE_CLIPBOARD_OFFER,
        TYPE_FILE_OFFER, TYPE_KEY_EVENT,
    };

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn rejects_channel_sig_signed_by_wrong_identity() {
        let server_id = DeviceIdentity::generate();
        let client_id = DeviceIdentity::generate();
        let wrong_signer = DeviceIdentity::generate();

        let server_cert = build_tls_certificate(&server_id).expect("server cert");
        let server_cfg =
            build_server_config(&server_cert, PinPolicy::Pinned(client_id.device_id()))
                .expect("server config");
        let server =
            quinn::Endpoint::server(server_cfg, crate::loopback_addr()).expect("server endpoint");
        let server_addr = server.local_addr().expect("server addr");

        let client = quinn::Endpoint::client(crate::loopback_addr()).expect("client endpoint");
        let client_cert = build_tls_certificate(&client_id).expect("client cert");
        let client_cfg =
            build_client_config(&client_cert, PinPolicy::Pinned(server_id.device_id()))
                .expect("client config");

        let accept = tokio::spawn(async move {
            let incoming = server
                .accept()
                .await
                .expect("incoming connection")
                .await
                .expect("accepted connection");
            let (send, mut recv) = incoming.accept_bi().await.expect("accept control");
            let seed = control::consume_prime(&mut recv)
                .await
                .expect("consume prime");
            let control = ControlStream::new(send, recv, seed);
            let result = exchange(&incoming, &control, &server_id).await;
            server.close(0u32.into(), b"done");
            result
        });

        let connection = client
            .connect_with(client_cfg, server_addr, "mouser")
            .expect("dial")
            .await
            .expect("connected");
        let (mut send, recv) = connection.open_bi().await.expect("open control");
        let prime =
            mouser_protocol::encode_frame(control::TYPE_STREAM_PRIME, 0, &[]).expect("prime frame");
        send.write_all(&prime).await.expect("write prime");
        let control = ControlStream::new(send, recv, Vec::new());

        let bad_hello = hello_signed_by(&client_id, &wrong_signer, &connection);
        send_hello(&control, &bad_hello)
            .await
            .expect("send bad hello");

        let rejected = tokio::time::timeout(Duration::from_secs(5), accept)
            .await
            .expect("accept completed")
            .expect("accept task");
        assert!(
            matches!(rejected, Err(NetError::Connect(ref reason)) if reason.contains("channel_sig")),
            "bad channel_sig must reject before an InteractiveConnection exists: {rejected:?}"
        );

        connection.close(0u32.into(), b"done");
        client.close(0u32.into(), b"done");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn rejects_runtime_frames_before_verified_hello() {
        let key_payload = to_cbor(&KeyEvent {
            usage: 0x04,
            down: true,
            mods: 0,
            owner_epoch: 1,
            ctr: 1,
        })
        .expect("key payload");
        let clip_payload = to_cbor(&ClipboardOffer {
            entries: Vec::new(),
            origin: vec![0xAA; 32],
        })
        .expect("clipboard payload");
        let file_payload = to_cbor(&FileOffer {
            transfer_id: 7,
            files: vec![FileEntry {
                name: "blocked.txt".to_string(),
                size: 1,
                sha256: None,
            }],
        })
        .expect("file payload");

        for (ty, payload) in [
            (TYPE_KEY_EVENT, key_payload),
            (TYPE_CLIPBOARD_OFFER, clip_payload),
            (TYPE_FILE_OFFER, file_payload),
        ] {
            let rejected = exchange_result_after_first_frame(ty, payload).await;
            assert!(
                matches!(rejected, Err(NetError::Connect(ref reason)) if reason.contains("expected Hello")),
                "runtime frame {ty:#06x} must be rejected before channel verification: {rejected:?}"
            );
        }
    }

    fn hello_signed_by(
        claimed: &DeviceIdentity,
        signer: &DeviceIdentity,
        connection: &Connection,
    ) -> Hello {
        let claimed_id = claimed.device_id();
        let to_sign = channel_exporter(connection, &claimed_id).expect("exporter");
        let signature: Signature = signer.sign(&to_sign);
        Hello {
            device_id: claimed_id.to_vec(),
            name: String::new(),
            os: Os::Macos,
            engine_version: "test".to_string(),
            capabilities: CapabilitySet::default(),
            role: Role::Eligible,
            session_id: 1,
            channel_sig: signature.to_vec(),
        }
    }

    async fn exchange_result_after_first_frame(
        msg_type: u16,
        payload: Vec<u8>,
    ) -> Result<VerifiedHello, NetError> {
        let server_id = DeviceIdentity::generate();
        let client_id = DeviceIdentity::generate();

        let server_cert = build_tls_certificate(&server_id).expect("server cert");
        let server_cfg =
            build_server_config(&server_cert, PinPolicy::Pinned(client_id.device_id()))
                .expect("server config");
        let server =
            quinn::Endpoint::server(server_cfg, crate::loopback_addr()).expect("server endpoint");
        let server_addr = server.local_addr().expect("server addr");

        let client = quinn::Endpoint::client(crate::loopback_addr()).expect("client endpoint");
        let client_cert = build_tls_certificate(&client_id).expect("client cert");
        let client_cfg =
            build_client_config(&client_cert, PinPolicy::Pinned(server_id.device_id()))
                .expect("client config");

        let accept = tokio::spawn(async move {
            let incoming = server
                .accept()
                .await
                .expect("incoming connection")
                .await
                .expect("accepted connection");
            let (send, mut recv) = incoming.accept_bi().await.expect("accept control");
            let seed = control::consume_prime(&mut recv)
                .await
                .expect("consume prime");
            let control = ControlStream::new(send, recv, seed);
            let result = exchange(&incoming, &control, &server_id).await;
            server.close(0u32.into(), b"done");
            result
        });

        let connection = client
            .connect_with(client_cfg, server_addr, "mouser")
            .expect("dial")
            .await
            .expect("connected");
        let (mut send, _recv) = connection.open_bi().await.expect("open control");
        let prime =
            mouser_protocol::encode_frame(control::TYPE_STREAM_PRIME, 0, &[]).expect("prime frame");
        send.write_all(&prime).await.expect("write prime");
        let frame = mouser_protocol::encode_frame(msg_type, 0, &payload).expect("runtime frame");
        send.write_all(&frame).await.expect("write first frame");

        let result = tokio::time::timeout(Duration::from_secs(5), accept)
            .await
            .expect("accept completed")
            .expect("accept task");
        connection.close(0u32.into(), b"done");
        client.close(0u32.into(), b"done");
        result
    }
}
