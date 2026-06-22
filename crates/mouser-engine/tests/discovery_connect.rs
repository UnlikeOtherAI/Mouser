//! End-to-end: mDNS discovery actually drives a QUIC connection. An "acceptor" binds a
//! server and advertises its real port over `_mouser._udp.local` (loopback); a "dialer"
//! browses, resolves the peer purely from mDNS, and dials the discovered address. Proves
//! the discovery → dial → device_id-pinned connect path works with no manual addressing.
//!
//! mDNS uses multicast; the loopback interface is enabled (off by default in `mdns-sd`)
//! so this does not need a routable network, with a generous timeout for resolve latency.

use std::time::Duration;

use mouser_engine::discovery;
use mouser_net::{Advertiser, Browser, DeviceIdentity, InteractiveEndpoint, PeerEvent, PinPolicy};

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mdns_discovery_drives_quic_connect() {
    let acceptor_id = DeviceIdentity::generate();
    let dialer_id = DeviceIdentity::generate();

    // Acceptor: bind a server, then advertise its REAL bound port over mDNS.
    let acceptor =
        InteractiveEndpoint::bind_server(&acceptor_id, mouser_net::loopback_addr(), PinPolicy::TrustOnFirstUse)
            .expect("bind acceptor");
    let iport = acceptor.local_addr().expect("acceptor addr").port();
    let advert = discovery::local_advert(&acceptor_id, "Acceptor", iport);
    let _advertiser = Advertiser::advertise_loopback(&advert).expect("advertise");

    let accept = tokio::spawn(async move {
        let conn = acceptor.accept_interactive().await.expect("accept");
        (acceptor, conn)
    });

    // Dialer: browse and resolve the acceptor purely from mDNS.
    let browser = Browser::browse_loopback().expect("browse");
    let want_id = acceptor_id.device_id_base32();
    let peer = tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            match browser.next_event().await {
                Some(PeerEvent::Found(p)) if p.id == want_id => return p,
                Some(_) => continue,
                None => panic!("browse channel closed before resolving the peer"),
            }
        }
    })
    .await
    .expect("mDNS discovery timed out");

    // The advertised TXT id decodes to the acceptor's real device_id, and it's dialable.
    assert_eq!(
        discovery::peer_device_id(&peer),
        Some(acceptor_id.device_id()),
        "advertised id decodes to the acceptor's device_id"
    );
    let addr = discovery::peer_socket_addr(&peer).expect("resolved a dialable addr from mDNS");

    // Dial the discovered address, pinning the discovered id (§3).
    let client = InteractiveEndpoint::bind_client(mouser_net::loopback_addr()).expect("bind client");
    let dialer_conn = client
        .connect_interactive(&dialer_id, addr, PinPolicy::Pinned(acceptor_id.device_id()))
        .await
        .expect("connect to the mDNS-discovered peer");
    let (_acceptor_ep, acceptor_conn) = accept.await.expect("accept task");

    // The connection formed from discovery alone, with mutual device_id pinning (§3).
    assert_eq!(dialer_conn.peer_device_id(), Some(acceptor_id.device_id()));
    assert_eq!(acceptor_conn.peer_device_id(), Some(dialer_id.device_id()));
}
