//! End-to-end input latency + throughput harness over a real QUIC interactive connection
//! on 127.0.0.1. Because both endpoints live in one process they share one monotonic
//! clock, so one-way latency is measured directly (no cross-machine clock sync). This
//! isolates the transport + serialization cost; it is a LOWER BOUND on real Win<->Mac lag
//! (it excludes NIC/Wi-Fi/driver), and the network half is measured separately via the
//! LAN RTT. Together they bound end-to-end input lag against the repo's <=5 ms wired /
//! <=15 ms Wi-Fi motion-to-injection SLO (docs/architecture.md).
//!
//! Run with numbers visible:
//!   cargo test -p mouser-net --test loopback_latency -- --nocapture --test-threads=1
//!
//! Latency asserts are deliberately generous (loopback is sub-millisecond, but CI runners
//! and Windows scheduler jitter can spike); the printed p50/p99 are the real evidence.

use std::sync::Arc;
use std::time::{Duration, Instant};

use mouser_net::{DeviceIdentity, InteractiveConnection, InteractiveEndpoint, PinPolicy};
use mouser_protocol::{from_cbor, to_cbor, Datagram, KeyEvent, PointerMotion, TYPE_KEY_EVENT};

/// Stand up two in-process endpoints on loopback, complete the §5 handshake (mutual
/// device-id pinning), and return both endpoints (kept alive) + both connection halves.
async fn setup() -> (
    InteractiveEndpoint,
    InteractiveConnection,
    InteractiveEndpoint,
    InteractiveConnection,
) {
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
    let client_conn = client
        .connect_interactive(&client_id, server_addr, PinPolicy::Pinned(server_device_id))
        .await
        .expect("client connect");
    let (server, server_conn) = accept.await.expect("accept task");
    (server, server_conn, client, client_conn)
}

fn us(d: Duration) -> f64 {
    d.as_secs_f64() * 1e6
}

/// Print sorted-latency percentiles in microseconds and return p99.
fn report(label: &str, mut lat: Vec<Duration>) -> Duration {
    lat.sort();
    let n = lat.len();
    if n == 0 {
        println!("{label}: NO SAMPLES");
        return Duration::ZERO;
    }
    let at = |p: f64| {
        let idx = (((n - 1) as f64) * p).round() as usize;
        lat.get(idx).copied().unwrap_or_default()
    };
    println!(
        "{label}: n={n}  p50={:.1}us  p90={:.1}us  p99={:.1}us  max={:.1}us",
        us(at(0.50)),
        us(at(0.90)),
        us(at(0.99)),
        us(at(1.00)),
    );
    at(0.99)
}

/// KEY one-way latency over the reliable control stream. The client sends a stream of
/// `KeyEvent` frames (naturally paced by `send_control`'s ack-await — one in flight at a
/// time, so no queueing); a drain task on the server timestamps arrival keyed by `ctr`.
/// Same-process clock => `recv_at - send_at` is true one-way application latency.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn loopback_key_one_way_latency() {
    let (_se, server_conn, _ce, client_conn) = setup().await;
    let server_conn = Arc::new(server_conn);

    const N: usize = 5_000;
    let drain_sc = Arc::clone(&server_conn);
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<(u64, Instant)>();
    let drain = tokio::spawn(async move {
        for _ in 0..N {
            match drain_sc.recv_control().await {
                Ok((_ty, body)) => {
                    let now = Instant::now();
                    let ke: KeyEvent = from_cbor(&body).expect("decode key");
                    let _ = tx.send((ke.ctr, now));
                }
                Err(e) => panic!("recv_control error: {e}"),
            }
        }
    });

    let mut send_at: Vec<Instant> = Vec::with_capacity(N);
    let wall0 = Instant::now();
    for i in 0..N {
        let ke = KeyEvent {
            usage: 0x04 + (i % 200) as u16,
            down: i % 2 == 0,
            mods: 0,
            owner_epoch: 1,
            ctr: i as u64,
        };
        let payload = to_cbor(&ke).expect("encode key");
        send_at.push(Instant::now());
        client_conn
            .send_control(TYPE_KEY_EVENT, &payload)
            .await
            .expect("send_control");
    }

    let mut lat = Vec::with_capacity(N);
    for _ in 0..N {
        let (ctr, recv_t) = rx.recv().await.expect("recv timestamp");
        if let Some(&s) = send_at.get(ctr as usize) {
            lat.push(recv_t.saturating_duration_since(s));
        }
    }
    let wall = wall0.elapsed();
    drain.await.expect("drain task");

    println!("\n=== loopback KEY one-way latency (reliable control stream) ===");
    let p99 = report("key one-way", lat);
    println!(
        "serial round-trip-paced rate: {:.0} keys/s over {:.2}s",
        N as f64 / wall.as_secs_f64(),
        wall.as_secs_f64()
    );
    println!("==============================================================\n");

    client_conn.close();
    server_conn.close();

    // Generous ceiling: loopback one-way is expected sub-millisecond; this only trips on a
    // catastrophic regression and tolerates CI/scheduler jitter.
    assert!(
        p99 < Duration::from_millis(25),
        "key one-way p99 = {:.1}us exceeds 25ms — transport/serialization lag regression",
        us(p99)
    );
}

/// MOTION one-way latency + delivery fraction over the unreliable datagram plane. Motion
/// is intentionally lossy + keep-newest coalescing, so we seq-tag each sample, pace the
/// sends so the pump can deliver distinct samples, and measure latency over DELIVERED
/// seqs only (loss/coalescing is by design for a cursor — the newest position wins).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn loopback_motion_latency_and_delivery() {
    let (_se, server_conn, _ce, client_conn) = setup().await;
    let server_conn = Arc::new(server_conn);

    const NM: u32 = 2_000;
    let drain_sc = Arc::clone(&server_conn);
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<(u32, Instant)>();
    let drain = tokio::spawn(async move {
        // Drain until 500ms of silence (the send loop has finished).
        loop {
            match tokio::time::timeout(Duration::from_millis(500), drain_sc.recv_motion()).await {
                Ok(Ok(Datagram::Motion(m))) => {
                    let _ = tx.send((m.seq, Instant::now()));
                }
                Ok(Ok(_)) => {}
                Ok(Err(_)) => break,
                Err(_) => break,
            }
        }
    });

    use std::collections::HashMap;
    let mut send_at: HashMap<u32, Instant> = HashMap::with_capacity(NM as usize);
    for seq in 1..=NM {
        let m = PointerMotion {
            owner_epoch: 1,
            seq,
            display_id: 0,
            x: (seq as i32) % 1920,
            y: (seq as i32) % 1080,
        };
        send_at.insert(seq, Instant::now());
        client_conn.send_motion(&m).expect("send motion");
        tokio::time::sleep(Duration::from_millis(1)).await; // ~1000 samples/s
    }
    drain.await.expect("drain task");

    let mut lat = Vec::new();
    let mut delivered = 0u32;
    while let Ok((seq, recv_t)) = rx.try_recv() {
        if let Some(s) = send_at.get(&seq) {
            lat.push(recv_t.saturating_duration_since(*s));
            delivered += 1;
        }
    }
    let frac = f64::from(delivered) / f64::from(NM) * 100.0;

    println!("\n=== loopback MOTION latency + delivery (unreliable datagrams, keep-newest) ===");
    println!("offered={NM}  delivered={delivered} ({frac:.1}% — coalescing/loss is by design)");
    let p99 = report("motion one-way", lat);
    println!("=============================================================================\n");

    client_conn.close();
    server_conn.close();

    assert!(
        delivered > NM / 2,
        "only {delivered}/{NM} motion samples delivered on loopback — pump/datagram regression"
    );
    assert!(
        p99 < Duration::from_millis(25),
        "motion one-way p99 = {:.1}us exceeds 25ms — datagram lag regression",
        us(p99)
    );
}
