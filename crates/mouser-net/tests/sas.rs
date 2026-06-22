//! §5 step-3 SAS pairing over a real loopback interactive connection. Mirrors
//! `tests/loopback.rs`: two QUIC endpoints in one process establish an interactive
//! connection (§6.1) with mutual cert pinning (§3), then **both** ends derive the
//! mandatory 6-digit Short Authentication String. The two strings MUST be equal and
//! be exactly six decimal digits — that equality is the whole point of the SAS (the user
//! compares the two screens), and it only holds if the TLS-exporter derivation is
//! symmetric and the ascending-id context is order-independent (§5 step 3).

use std::time::Duration;

use mouser_net::{DeviceIdentity, InteractiveEndpoint, PinPolicy};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn both_ends_derive_identical_six_digit_sas() {
    // Both peers generate a permanent Ed25519 identity (§3).
    let server_id = DeviceIdentity::generate();
    let client_id = DeviceIdentity::generate();
    let server_device_id = server_id.device_id();
    let client_device_id = client_id.device_id();
    assert_ne!(
        server_device_id, client_device_id,
        "two fresh identities must differ"
    );

    // Mutual cert pinning (§3): server pins the client, client pins the server.
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

    let client_conn = client
        .connect_interactive(&client_id, server_addr, PinPolicy::Pinned(server_device_id))
        .await
        .expect("client connect");

    let (_server_endpoint, server_conn) = accept.await.expect("accept task");

    // Each end computes its SAS from its OWN local id + the peer's pinned id (§5 step 3).
    let client_sas = client_conn.sas().expect("client SAS");
    let server_sas = server_conn.sas().expect("server SAS");

    // The whole security property: both screens show the same code.
    assert_eq!(
        client_sas, server_sas,
        "both ends MUST derive the identical SAS (§5 step 3) — got client={client_sas} server={server_sas}"
    );

    // It is exactly six decimal digits.
    assert_eq!(
        client_sas.len(),
        6,
        "SAS must be 6 digits, got {client_sas:?}"
    );
    assert!(
        client_sas.chars().all(|c| c.is_ascii_digit()),
        "SAS must be all decimal digits, got {client_sas:?}"
    );

    // Recomputing on the same connection is deterministic (stable across the session).
    assert_eq!(
        client_conn.sas().expect("client SAS again"),
        client_sas,
        "SAS must be stable for the connection's lifetime"
    );

    // Sanity: a tight timeout proves nothing here blocks — SAS is a pure CPU derivation.
    let quick = tokio::time::timeout(Duration::from_secs(1), async { server_conn.sas() })
        .await
        .expect("SAS computation must not block");
    assert_eq!(quick.expect("server SAS quick"), server_sas);

    client_conn.close();
    server_conn.close();
}
