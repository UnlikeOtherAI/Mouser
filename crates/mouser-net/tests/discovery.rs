//! Discovery integration (§4): advertise a `_mouser._udp.local` service on loopback,
//! then browse and confirm we find our own service and parse its TXT keys.
//!
//! mDNS uses multicast; this test enables the loopback interface (disabled by default
//! in `mdns-sd`) so it does not depend on a routable network. It is given a generous
//! timeout to absorb mDNS announce/resolve latency.

use std::time::Duration;

use mouser_net::discovery::{Advertiser, Browser};
use mouser_net::{PeerAdvert, PeerEvent};

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
        // Populated from the resolved mDNS address records on the receive side; the
        // advertised value is not what's transmitted (the daemon owns A/AAAA records).
        addrs: Vec::new(),
    };

    let _advertiser = Advertiser::advertise_loopback(&advert).expect("advertise");
    let browser = Browser::browse_loopback().expect("browse");

    let found = tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            match browser.next_event().await {
                Some(PeerEvent::Found(peer)) if peer.id == advert.id => return peer,
                Some(_) => continue,
                None => panic!("browse channel closed before finding our service"),
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
    // C2-6: a `Found` peer carries at least one resolved, dialable address (loopback).
    assert!(
        !found.addrs.is_empty(),
        "a Found peer must carry a dialable address (C2-6)"
    );
}

/// C2-6: a departing peer must surface as [`PeerEvent::Removed`]. We resolve our own
/// advertisement, then unregister it (which sends an mDNS goodbye) and assert the browser
/// yields a `Removed` event. Bounded by a timeout so a regression to the old
/// resolved-only loop fails loudly instead of hanging.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn browse_surfaces_departures_as_removed() {
    let advert = PeerAdvert {
        id: "removed99removed99removed99removed".to_string(),
        name: "Going Away".to_string(),
        os: "linux".to_string(),
        ver: "0.0.0".to_string(),
        iport: 41000,
        bport: 41001,
        caps: String::new(),
        role: "ineligible".to_string(),
        addrs: Vec::new(),
    };

    let advertiser = Advertiser::advertise_loopback(&advert).expect("advertise");
    let browser = Browser::browse_loopback().expect("browse");

    // First resolve it so the daemon knows the instance, then drop it.
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            match browser.next_event().await {
                Some(PeerEvent::Found(peer)) if peer.id == advert.id => return,
                Some(_) => continue,
                None => panic!("browse channel closed before resolving"),
            }
        }
    })
    .await
    .expect("timed out resolving our own service");

    // Unregister → mDNS goodbye → the daemon should emit ServiceRemoved.
    advertiser.unregister().expect("unregister");

    // Match THIS test's instance specifically: other discovery tests share the loopback
    // mDNS namespace in-process, so unrelated `Removed` events can interleave. The
    // instance fullname embeds `"<name> (<short id>)"` (see `PeerAdvert::instance_name`).
    let want = advert.instance_name();
    let removed = tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            match browser.next_event().await {
                Some(PeerEvent::Removed(fullname)) if fullname.contains(&want) => return fullname,
                Some(_) => continue,
                None => panic!("browse channel closed before departure surfaced"),
            }
        }
    })
    .await
    .expect("departure (Removed) for our instance never surfaced (C2-6)");

    assert!(
        removed.contains(&want),
        "Removed event names the departed instance, got {removed:?}"
    );
}
