//! Discovery integration (§4): advertise a `_mouser._udp.local` service on loopback,
//! then browse and confirm we find our own service and parse its TXT keys.
//!
//! mDNS uses multicast; this test enables the loopback interface (disabled by default
//! in `mdns-sd`) so it does not depend on a routable network. It is given a generous
//! timeout to absorb mDNS announce/resolve latency.

use std::time::Duration;

use mouser_net::discovery::{Advertiser, Browser};
use mouser_net::PeerAdvert;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn advertise_then_browse_finds_own_service_and_parses_txt() {
    let advert = PeerAdvert {
        id: "abcdefgh23456789abcdefgh23456789".to_string(),
        name: "Test Mac".to_string(),
        os: "macos".to_string(),
        ver: "0.0.0".to_string(),
        iport: 51820,
        bport: 51821,
        caps: "keyboard,mouse".to_string(),
        role: "eligible".to_string(),
    };

    let _advertiser = Advertiser::advertise_loopback(&advert).expect("advertise");
    let browser = Browser::browse_loopback().expect("browse");

    let found = tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            if let Some(peer) = browser.next_peer().await {
                if peer.id == advert.id {
                    return peer;
                }
            } else {
                panic!("browse channel closed before finding our service");
            }
        }
    })
    .await
    .expect("discovery timed out waiting for our own service");

    // TXT keys (§4) parsed back exactly as advertised.
    assert_eq!(found.id, advert.id);
    assert_eq!(found.name, advert.name);
    assert_eq!(found.os, advert.os);
    assert_eq!(found.ver, advert.ver);
    assert_eq!(found.iport, advert.iport);
    assert_eq!(found.bport, advert.bport);
    assert_eq!(found.caps, advert.caps);
    assert_eq!(found.role, advert.role);
}
