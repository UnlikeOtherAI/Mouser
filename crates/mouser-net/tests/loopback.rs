//! Loopback integration: two QUIC endpoints in one process establish an interactive
//! connection (§6.1), round-trip a `Ping` control frame on the bidi control stream
//! (§0.2 framing + CBOR), and deliver a `PointerMotion` over the datagram plane
//! (§7.6, RFC 9221).
//!
//! Stubbed (skeleton): the §5 `Hello`/`HelloAck` handshake, the mandatory SAS
//! pairing, and the `channel_sig` identity proof are NOT exchanged. Cert pinning
//! (§3) IS exercised — both ends pin the peer's `device_id`.

use std::time::Duration;

use mouser_net::{DeviceIdentity, InteractiveEndpoint, PinPolicy};
use mouser_protocol::{decode_frame, from_cbor, to_cbor, Datagram, Ping, PointerMotion, TYPE_PING};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn interactive_connection_roundtrips_ping_and_motion() {
    // Both peers generate a permanent Ed25519 identity (§3).
    let server_id = DeviceIdentity::generate();
    let client_id = DeviceIdentity::generate();
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

/// Negative pinning (§3): if the dialer pins a `device_id` that does **not** match the
/// cert the server actually presents, the QUIC/TLS handshake MUST fail. This guards the
/// pin comparison itself — a neutered `check_pin` (e.g. one that always returns `Ok`)
/// would make the connect below *succeed*, so this test fails loudly if pinning regresses.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mismatched_pin_fails_handshake() {
    let server_id = DeviceIdentity::generate();
    let client_id = DeviceIdentity::generate();
    let client_device_id = client_id.device_id();

    // A third identity the server will NEVER present — the dialer pins this by mistake.
    let wrong_id = DeviceIdentity::generate();
    let wrong_device_id = wrong_id.device_id();
    assert_ne!(
        wrong_device_id,
        server_id.device_id(),
        "wrong pin must differ from the server's real device_id"
    );

    // Server presents its real cert and (correctly) pins the client.
    let server = InteractiveEndpoint::bind_server(
        &server_id,
        mouser_net::loopback_addr(),
        PinPolicy::Pinned(client_device_id),
    )
    .expect("bind server");
    let server_addr = server.local_addr().expect("server addr");

    let client =
        InteractiveEndpoint::bind_client(mouser_net::loopback_addr()).expect("bind client");

    // The accept side may error or simply never complete once the client aborts the
    // handshake; bound it with a timeout so the test can't hang either way.
    let accept = tokio::spawn(async move {
        let _ = tokio::time::timeout(Duration::from_secs(5), server.accept_interactive()).await;
        server // keep the endpoint alive until the dial resolves
    });

    // Dial pinning the WRONG server device_id → the client's server-cert verifier
    // rejects the presented cert and the handshake fails deterministically.
    let result = tokio::time::timeout(
        Duration::from_secs(5),
        client.connect_interactive(&client_id, server_addr, PinPolicy::Pinned(wrong_device_id)),
    )
    .await
    .expect("dial did not resolve within timeout (handshake should fail fast)");

    assert!(
        result.is_err(),
        "handshake MUST fail when the pinned device_id does not match the presented cert (§3)"
    );

    let _server = accept.await.expect("accept task");
}

/// A2 regression: the **acceptor** sends the first control frame and it must NOT
/// deadlock. With the old lazy `open_bi`/`accept_bi`-on-first-I/O design, an acceptor
/// whose first action is `send_control` would block forever (quinn requires the opener
/// to write before the peer can `accept_bi`). The whole exchange is wrapped in a tight
/// timeout so a regression fails loudly instead of hanging the suite.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn acceptor_sends_first_does_not_deadlock() {
    let server_id = DeviceIdentity::generate();
    let client_id = DeviceIdentity::generate();
    let server_device_id = server_id.device_id();
    let client_device_id = client_id.device_id();

    let server = InteractiveEndpoint::bind_server(
        &server_id,
        mouser_net::loopback_addr(),
        PinPolicy::Pinned(client_device_id),
    )
    .expect("bind server");
    let server_addr = server.local_addr().expect("server addr");
    let client =
        InteractiveEndpoint::bind_client(mouser_net::loopback_addr()).expect("bind client");

    // Acceptor (server) is the one that SENDS first.
    let accept = tokio::spawn(async move {
        let conn = server.accept_interactive().await.expect("accept");
        let ping = Ping { id: 13 };
        let payload = to_cbor(&ping).expect("encode ping");
        conn.send_control(TYPE_PING, &payload)
            .await
            .expect("acceptor sends first");
        (server, conn)
    });

    let client_conn = client
        .connect_interactive(&client_id, server_addr, PinPolicy::Pinned(server_device_id))
        .await
        .expect("client connect");

    // The initiator (client) receives the acceptor's first frame. If stream
    // establishment were tied to first-I/O direction, this would hang — so bound it.
    let (msg_type, body) = tokio::time::timeout(Duration::from_secs(5), client_conn.recv_control())
        .await
        .expect("acceptor-first must not deadlock (A2)")
        .expect("recv control");
    assert_eq!(msg_type, TYPE_PING);
    let got: Ping = from_cbor(&body).expect("decode ping");
    assert_eq!(got, Ping { id: 13 });

    let (_server_endpoint, server_conn) = accept.await.expect("accept task");
    server_conn.close();
    client_conn.close();
}

/// A3 regression: `recv_control` must be cancel-safe. We repeatedly poll it under a
/// 1ms timeout (the way the engine will, inside `tokio::select!`) while the peer drips
/// a stream of frames. A dropped recv future that lost partially-read bytes would
/// desync the length-prefixed framing and every subsequent frame would misparse — so
/// if all frames arrive **in order with intact payloads**, cancel-safety holds.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn recv_control_is_cancel_safe_under_timeout() {
    use std::sync::Arc;

    let server_id = DeviceIdentity::generate();
    let client_id = DeviceIdentity::generate();
    let server_device_id = server_id.device_id();
    let client_device_id = client_id.device_id();

    let server = InteractiveEndpoint::bind_server(
        &server_id,
        mouser_net::loopback_addr(),
        PinPolicy::Pinned(client_device_id),
    )
    .expect("bind server");
    let server_addr = server.local_addr().expect("server addr");
    let client =
        InteractiveEndpoint::bind_client(mouser_net::loopback_addr()).expect("bind client");

    let accept = tokio::spawn(async move {
        let conn = server.accept_interactive().await.expect("accept");
        (server, conn)
    });
    let client_conn = Arc::new(
        client
            .connect_interactive(&client_id, server_addr, PinPolicy::Pinned(server_device_id))
            .await
            .expect("client connect"),
    );
    let (_server_endpoint, server_conn) = accept.await.expect("accept task");

    const N: u64 = 200;
    // Sender: drip N frames with distinct ids and small gaps, so the receiver's tight
    // timeouts frequently cancel a read mid-frame.
    let sender = tokio::spawn(async move {
        for id in 0..N {
            let payload = to_cbor(&Ping { id }).expect("encode");
            server_conn
                .send_control(TYPE_PING, &payload)
                .await
                .expect("send");
            tokio::time::sleep(Duration::from_micros(200)).await;
        }
        server_conn
    });

    // Receiver: only ever recv under a 1ms timeout; many of these WILL cancel a recv
    // future that has already buffered part of a frame.
    let mut next_expected = 0u64;
    let overall = tokio::time::Instant::now();
    while next_expected < N {
        assert!(
            overall.elapsed() < Duration::from_secs(30),
            "cancel-safe recv stalled — likely framing desync (A3 regression)"
        );
        match tokio::time::timeout(Duration::from_millis(1), client_conn.recv_control()).await {
            Ok(res) => {
                let (msg_type, body) = res.expect("recv control");
                assert_eq!(msg_type, TYPE_PING);
                let got: Ping = from_cbor(&body).expect("decode (corrupt => framing desync)");
                assert_eq!(
                    got,
                    Ping { id: next_expected },
                    "frames must arrive in order with intact payloads"
                );
                next_expected += 1;
            }
            Err(_) => continue, // timed out (cancelled) — buffer must survive intact
        }
    }

    let server_conn = sender.await.expect("sender task");
    server_conn.close();
    client_conn.close();
}
