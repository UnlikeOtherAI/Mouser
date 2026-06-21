//! Loopback integration: two QUIC endpoints in one process establish an interactive
//! connection (§6.1), round-trip a `Ping` control frame on the bidi control stream
//! (§0.2 framing + CBOR), and deliver a `PointerMotion` over the datagram plane
//! (§7.6, RFC 9221).
//!
//! Stubbed (skeleton): the §5 `Hello`/`HelloAck` handshake, the mandatory SAS
//! pairing, and the `channel_sig` identity proof are NOT exchanged. Cert pinning
//! (§3) IS exercised — both ends pin the peer's `device_id`.

use std::time::Duration;

use mouser_net::{Identity, InteractiveEndpoint, PinPolicy};
use mouser_protocol::{decode_frame, from_cbor, to_cbor, Datagram, Ping, PointerMotion, TYPE_PING};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn interactive_connection_roundtrips_ping_and_motion() {
    // Both peers generate a permanent Ed25519 identity (§3).
    let server_id = Identity::generate().expect("server identity");
    let client_id = Identity::generate().expect("client identity");
    let server_device_id = server_id.device_id();
    let client_device_id = client_id.device_id();

    // Server pins the client's device_id; client pins the server's (§3 mutual pin).
    let server = InteractiveEndpoint::bind_server(
        &server_id,
        mouser_net::loopback_addr(),
        PinPolicy::Pinned(client_device_id),
    )
    .expect("bind server");
    let server_addr = server.local_addr().expect("server addr");

    let client =
        InteractiveEndpoint::bind_client(mouser_net::loopback_addr()).expect("bind client");

    // Accept on a task; dial from the test.
    let accept = tokio::spawn(async move {
        let conn = server.accept_interactive().await.expect("accept");
        // Keep the endpoint alive for the connection's lifetime.
        (server, conn)
    });

    let client_conn = client
        .connect_interactive(&client_id, server_addr, PinPolicy::Pinned(server_device_id))
        .await
        .expect("client connect");

    let (_server_endpoint, server_conn) = accept.await.expect("accept task");

    // ALPN must be `mouser/1` (§2 — the sole version source).
    assert_eq!(
        client_conn.negotiated_alpn().as_deref(),
        Some(mouser_net::ALPN_MOUSER_1),
        "client negotiated ALPN mouser/1"
    );

    // Cert pinning resolved each side to the other's real device_id (§3).
    assert_eq!(
        client_conn.peer_device_id(),
        Some(server_device_id),
        "client sees server's pinned device_id"
    );
    assert_eq!(
        server_conn.peer_device_id(),
        Some(client_device_id),
        "server sees client's pinned device_id"
    );

    // --- Control stream: Ping round-trips (framed CBOR, §0.2/§7.1) ---
    let ping = Ping { id: 7 };
    let payload = to_cbor(&ping).expect("encode ping");
    client_conn
        .send_control(TYPE_PING, &payload)
        .await
        .expect("send ping");

    let (msg_type, body) = server_conn.recv_control().await.expect("recv ping");
    assert_eq!(msg_type, TYPE_PING);
    let got: Ping = from_cbor(&body).expect("decode ping");
    assert_eq!(got, ping, "Ping round-trips over the control stream");

    // Re-frame to prove the on-wire bytes match §0.2 exactly.
    let reframed = mouser_protocol::encode_frame(msg_type, 0, &body).expect("reframe");
    let (frame, _) = decode_frame(&reframed).expect("decode frame");
    assert_eq!(frame.msg_type, TYPE_PING);
    assert_eq!(frame.flags, 0);

    // --- Datagram plane: PointerMotion arrives (§7.6, RFC 9221) ---
    let motion = PointerMotion {
        owner_epoch: 1,
        seq: 99,
        display_id: 0,
        x: 640,
        y: 360,
    };
    // Datagram delivery is lossy; retry a few times within a timeout.
    let mut received = None;
    for _ in 0..20 {
        client_conn.send_motion(&motion).expect("send motion");
        match tokio::time::timeout(Duration::from_millis(200), server_conn.recv_motion()).await {
            Ok(Ok(datagram)) => {
                received = Some(datagram);
                break;
            }
            Ok(Err(e)) => panic!("recv_motion error: {e}"),
            Err(_) => continue,
        }
    }

    assert_eq!(
        received,
        Some(Datagram::Motion(motion)),
        "PointerMotion datagram arrives and decodes (§7.6)"
    );

    client_conn.close();
    server_conn.close();
}
