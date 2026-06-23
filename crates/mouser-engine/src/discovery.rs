//! mDNS-driven peer discovery for the engine (spec §4).
//!
//! Two `mouserd` instances on a LAN find each other automatically: each advertises a
//! `_mouser._udp.local` service carrying its `device_id` (base32) and interactive
//! port, and browses for the other. These helpers turn a resolved [`PeerAdvert`] into
//! the dialable `(device_id, SocketAddr)` the transport needs, and build this device's
//! own advertisement. Discovery is advisory: trust still comes from the §3 cert pin
//! (and, in the future, §5 SAS pairing) — never from the TXT record.

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr, UdpSocket};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use data_encoding::BASE32_NOPAD;
use mouser_core::{DeviceId, DeviceIdentity};
use mouser_net::{Browser, PeerAdvert, PeerEvent};
use tokio::sync::watch;

/// OS string advertised in the §4 TXT record.
const OS_NAME: &str = if cfg!(target_os = "macos") {
    "macos"
} else if cfg!(target_os = "windows") {
    "windows"
} else {
    "linux"
};

/// Build this device's advertisement (§4). `iport` is the bound interactive UDP port;
/// `bport` is the bound bulk UDP port.
pub fn local_advert(identity: &DeviceIdentity, name: &str, iport: u16, bport: u16) -> PeerAdvert {
    PeerAdvert {
        id: identity.device_id_base32(),
        name: name.to_string(),
        os: OS_NAME.to_string(),
        ver: env!("CARGO_PKG_VERSION").to_string(),
        iport,
        bport,
        caps: "keyboard,mouse,clipboard,files".to_string(),
        role: "eligible".to_string(),
        addrs: Vec::new(),
    }
}

/// Decode a base32 (no-pad, lowercase) `device_id` as produced by
/// [`DeviceIdentity::device_id_base32`]. `None` if it is not a valid 32-byte id.
pub fn decode_device_id(base32: &str) -> Option<DeviceId> {
    let bytes = BASE32_NOPAD
        .decode(base32.trim().to_uppercase().as_bytes())
        .ok()?;
    <[u8; 32]>::try_from(bytes.as_slice()).ok()
}

/// The peer's `device_id` from its advertisement, if the TXT `id` is a valid id.
pub fn peer_device_id(advert: &PeerAdvert) -> Option<DeviceId> {
    decode_device_id(&advert.id)
}

/// The first dialable socket address for a peer (resolved IP + interactive port).
/// `None` if the peer advertised no address or no interactive port.
pub fn peer_socket_addr(advert: &PeerAdvert) -> Option<SocketAddr> {
    if advert.iport == 0 {
        return None;
    }
    advert
        .addrs
        .first()
        .map(|ip| SocketAddr::new(*ip, advert.iport))
}

/// The first dialable socket address for a peer's bulk endpoint.
/// `None` if the peer advertised no address or no bulk port.
pub fn peer_bulk_socket_addr(advert: &PeerAdvert) -> Option<SocketAddr> {
    if advert.bport == 0 {
        return None;
    }
    advert
        .addrs
        .first()
        .map(|ip| SocketAddr::new(*ip, advert.bport))
}

/// Best-effort primary outbound IPv4 of this host, to advertise an A record on the
/// LAN. Uses the connect-a-UDP-socket trick — no packets are ever sent. `None` if it
/// can't be determined (e.g. no network).
pub fn local_ipv4() -> Option<IpAddr> {
    let sock = UdpSocket::bind("0.0.0.0:0").ok()?;
    // TEST-NET-3 (RFC 5737) discard port: selects the default route, transmits nothing.
    sock.connect("203.0.113.1:9").ok()?;
    sock.local_addr().ok().map(|addr| addr.ip())
}

/// A live registry of mDNS-discovered peers, fed by a single browse and read by every
/// consumer (the auto/IPC dialer and the IPC snapshot). One registry per host keeps all
/// discovery on a single [`mouser_net::Discovery`] daemon — multiple browsing daemons on
/// one host race for inbound multicast and silently drop peers (macOS).
#[derive(Clone)]
pub struct PeerRegistry {
    inner: Arc<Inner>,
}

struct Inner {
    /// Discovered peers keyed by DNS-SD instance fullname.
    peers: Mutex<HashMap<String, PeerAdvert>>,
    /// Bumped on every change so consumers can await updates without polling.
    version: watch::Sender<u64>,
}

impl Default for PeerRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl PeerRegistry {
    /// An empty registry.
    pub fn new() -> Self {
        let (version, _rx) = watch::channel(0);
        Self {
            inner: Arc::new(Inner {
                peers: Mutex::new(HashMap::new()),
                version,
            }),
        }
    }

    fn peers_guard(&self) -> MutexGuard<'_, HashMap<String, PeerAdvert>> {
        self.inner
            .peers
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// A snapshot of the currently discovered peers.
    pub fn peers(&self) -> Vec<PeerAdvert> {
        self.peers_guard().values().cloned().collect()
    }

    /// The discovered advert for `peer_id`, if it is currently visible.
    pub fn find(&self, peer_id: &DeviceId) -> Option<PeerAdvert> {
        self.peers_guard()
            .values()
            .find(|advert| peer_device_id(advert).as_ref() == Some(peer_id))
            .cloned()
    }

    /// A receiver that resolves on every registry change, for change-driven loops that
    /// re-scan [`PeerRegistry::peers`]/[`PeerRegistry::find`] without busy-polling.
    pub fn subscribe(&self) -> watch::Receiver<u64> {
        self.inner.version.subscribe()
    }

    /// Fold one browse event into the registry; returns whether the peer set changed.
    fn apply(&self, event: PeerEvent) -> bool {
        let changed = match event {
            PeerEvent::Found(advert) => {
                let key = format!("{}.{}", advert.instance_name(), mouser_net::SERVICE_TYPE);
                let mut guard = self.peers_guard();
                // A re-announce with new address/port counts as a change so dialers and
                // snapshots stay current; an identical re-announce does not.
                let changed = guard.get(&key) != Some(&advert);
                guard.insert(key, advert);
                changed
            }
            PeerEvent::Removed(fullname) => self.peers_guard().remove(&fullname).is_some(),
        };
        if changed {
            self.inner.version.send_modify(|v| *v = v.wrapping_add(1));
        }
        changed
    }
}

/// Drive `browser` into `registry` forever — the single consumer of the browse stream
/// (mdns-sd allows one querier per service type). Returns when the browse channel
/// closes (the owning [`mouser_net::Discovery`] was dropped).
pub async fn run_registry(browser: Browser, registry: PeerRegistry) {
    while let Some(event) = browser.next_event().await {
        registry.apply(event);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

    #[test]
    fn device_id_base32_round_trips_through_decode() {
        // Decoding what `device_id_base32` produced must recover the exact id, proving
        // our decoder matches mouser-core's encoder (the dial-pin path depends on it).
        let identity = DeviceIdentity::generate();
        let encoded = identity.device_id_base32();
        assert_eq!(decode_device_id(&encoded), Some(identity.device_id()));
    }

    #[test]
    fn decode_rejects_malformed_ids() {
        assert_eq!(decode_device_id("not base32 !!!"), None);
        // Valid base32 but the wrong length is not a 32-byte device id.
        assert_eq!(decode_device_id("aaaa"), None);
    }

    #[test]
    fn peer_socket_addr_pairs_first_address_with_iport() {
        let mut advert = local_advert(&DeviceIdentity::generate(), "Peer", 51820, 51821);
        assert_eq!(advert.bport, 51821);
        assert_eq!(
            peer_socket_addr(&advert),
            None,
            "no address yet → not dialable"
        );
        advert.addrs = vec![IpAddr::V4(Ipv4Addr::new(192, 168, 1, 50))];
        assert_eq!(
            peer_socket_addr(&advert),
            Some(SocketAddr::from(([192, 168, 1, 50], 51820))),
        );
        advert.iport = 0;
        assert_eq!(
            peer_socket_addr(&advert),
            None,
            "no interactive port → not dialable"
        );
    }

    #[test]
    fn peer_bulk_socket_addr_pairs_first_address_with_bport() {
        let mut advert = local_advert(&DeviceIdentity::generate(), "Peer", 51820, 51821);
        assert_eq!(peer_bulk_socket_addr(&advert), None);
        advert.addrs = vec![IpAddr::V4(Ipv4Addr::new(192, 168, 1, 50))];
        assert_eq!(
            peer_bulk_socket_addr(&advert),
            Some(SocketAddr::from(([192, 168, 1, 50], 51821))),
        );
        advert.bport = 0;
        assert_eq!(peer_bulk_socket_addr(&advert), None);
    }

    #[test]
    fn peer_device_id_reads_the_advert_id() {
        let identity = DeviceIdentity::generate();
        let advert = local_advert(&identity, "Peer", 1, 2);
        assert_eq!(peer_device_id(&advert), Some(identity.device_id()));
    }
}
