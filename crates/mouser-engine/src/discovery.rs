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
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};
use std::time::{Duration, Instant};

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

const LOCAL_IPV4_TTL: Duration = Duration::from_secs(5);

#[derive(Default)]
struct LocalIpv4Cache {
    cached: Mutex<Option<CachedLocalIpv4>>,
}

#[derive(Clone, Copy)]
struct CachedLocalIpv4 {
    checked_at: Instant,
    addr: Option<IpAddr>,
}

impl LocalIpv4Cache {
    fn get_or_probe(&self, now: Instant, probe: impl FnOnce() -> Option<IpAddr>) -> Option<IpAddr> {
        let mut guard = self.cached.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(cached) = guard.as_ref() {
            if now.saturating_duration_since(cached.checked_at) < LOCAL_IPV4_TTL {
                return cached.addr;
            }
        }
        let addr = probe();
        *guard = Some(CachedLocalIpv4 {
            checked_at: now,
            addr,
        });
        addr
    }
}

fn local_ipv4_cache() -> &'static LocalIpv4Cache {
    static CACHE: OnceLock<LocalIpv4Cache> = OnceLock::new();
    CACHE.get_or_init(LocalIpv4Cache::default)
}

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

/// The best dialable socket address for a peer's interactive endpoint (resolved IP +
/// interactive port). `None` if the peer advertised no usable address or no interactive
/// port. Prefers a routable address over a link-local IPv6 — see [`best_dialable_ip`].
pub fn peer_socket_addr(advert: &PeerAdvert) -> Option<SocketAddr> {
    if advert.iport == 0 {
        return None;
    }
    best_dialable_ip(&advert.addrs).map(|ip| SocketAddr::new(ip, advert.iport))
}

/// The best dialable socket address for a peer's bulk endpoint.
/// `None` if the peer advertised no usable address or no bulk port.
pub fn peer_bulk_socket_addr(advert: &PeerAdvert) -> Option<SocketAddr> {
    if advert.bport == 0 {
        return None;
    }
    best_dialable_ip(&advert.addrs).map(|ip| SocketAddr::new(ip, advert.bport))
}

/// ALL dialable socket addresses for a peer's interactive endpoint, ordered
/// most-reachable-first (routable IPv4, then routable IPv6), dropping bare link-local
/// IPv6 (unreachable without a scope id — it only burns a dial timeout). Empty if the
/// peer has no interactive port or no usable address. Feeds the ordered multi-address
/// dialer ([`mouser_net::InteractiveEndpoint::connect_interactive_any`], which tries each
/// candidate in turn under a per-address timeout) so a peer that resolved to a family it
/// isn't listening on (e.g. an IPv6-only mDNS resolution of an IPv4-only Windows peer) is
/// recovered by trying the other address instead of timing out.
pub fn peer_socket_addrs(advert: &PeerAdvert) -> Vec<SocketAddr> {
    if advert.iport == 0 {
        return Vec::new();
    }
    ordered_dialable_ips(&advert.addrs)
        .into_iter()
        .map(|ip| SocketAddr::new(ip, advert.iport))
        .collect()
}

/// ALL dialable socket addresses for a peer's **bulk** endpoint, ordered most-reachable-
/// first — the bulk-plane analogue of [`peer_socket_addrs`]. Feeds
/// [`mouser_net::BulkEndpoint::connect_bulk_any`] so a wrong-family mDNS resolution doesn't
/// hang the clipboard/file dial on the idle timeout. Empty if the peer has no bulk port or
/// no usable address.
pub fn peer_bulk_socket_addrs(advert: &PeerAdvert) -> Vec<SocketAddr> {
    if advert.bport == 0 {
        return Vec::new();
    }
    ordered_dialable_ips(&advert.addrs)
        .into_iter()
        .map(|ip| SocketAddr::new(ip, advert.bport))
        .collect()
}

/// `addrs` ordered for dialing: routable IPv4 first (most reliable on a LAN), then
/// routable IPv6; loopback / unspecified / bare link-local IPv6 are dropped.
fn ordered_dialable_ips(addrs: &[IpAddr]) -> Vec<IpAddr> {
    let mut ordered: Vec<IpAddr> = addrs
        .iter()
        .copied()
        .filter(|ip| matches!(ip, IpAddr::V4(v4) if !v4.is_loopback() && !v4.is_link_local() && !v4.is_unspecified()))
        .collect();
    ordered.extend(
        addrs
            .iter()
            .copied()
            .filter(|ip| matches!(ip, IpAddr::V6(v6) if !v6.is_loopback() && !v6.is_unicast_link_local() && !v6.is_unspecified())),
    );
    ordered
}

/// Choose which advertised IP to dial. A peer commonly advertises several addresses at
/// once — an IPv4, a global IPv6, and a link-local IPv6 — and the order is not
/// meaningful. A bare link-local `fe80::/10` is NOT reachable from another host without
/// an interface scope id, so dialing it just hangs until timeout (the cause of a stuck
/// "connecting"). Prefer, in order: a routable IPv4 (most reliable on a LAN), then a
/// routable IPv6, falling back to the first advertised address only when nothing better
/// exists — so a peer that advertises any routable address is always dialed on it.
fn best_dialable_ip(addrs: &[IpAddr]) -> Option<IpAddr> {
    let routable_v4 = addrs.iter().find(|ip| match ip {
        IpAddr::V4(v4) => !v4.is_loopback() && !v4.is_link_local() && !v4.is_unspecified(),
        IpAddr::V6(_) => false,
    });
    if let Some(ip) = routable_v4 {
        return Some(*ip);
    }
    let routable_v6 = addrs.iter().find(|ip| match ip {
        IpAddr::V6(v6) => !v6.is_loopback() && !v6.is_unicast_link_local() && !v6.is_unspecified(),
        IpAddr::V4(_) => false,
    });
    if let Some(ip) = routable_v6 {
        return Some(*ip);
    }
    addrs.first().copied()
}

/// Best-effort primary outbound IPv4 of this host, to advertise an A record on the
/// LAN. Uses the connect-a-UDP-socket trick — no packets are ever sent. `None` if it
/// can't be determined (e.g. no network).
pub fn local_ipv4() -> Option<IpAddr> {
    local_ipv4_cache().get_or_probe(Instant::now(), probe_local_ipv4)
}

fn probe_local_ipv4() -> Option<IpAddr> {
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
            PeerEvent::Found(mut advert) => {
                let key = format!("{}.{}", advert.instance_name(), mouser_net::SERVICE_TYPE);
                let mut guard = self.peers_guard();
                // mdns-sd often resolves a peer's A and AAAA records in separate events.
                // Replacing the entry each time would drop one family, so the dialer's
                // chosen address flaps between IPv4 and IPv6 and times out whenever it
                // lands on a family the peer isn't listening on. Merge instead: keep the
                // latest TXT/port but the *union* of all resolved addresses, ordered
                // IPv4-first so `best_dialable_ip` prefers the most LAN-reliable family.
                // (A peer that genuinely leaves clears its entry via Removed.)
                if let Some(prev) = guard.get(&key) {
                    for ip in &prev.addrs {
                        if !advert.addrs.contains(ip) {
                            advert.addrs.push(*ip);
                        }
                    }
                }
                advert
                    .addrs
                    .sort_by_key(|ip| (ip.is_ipv6(), ip.to_string()));
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
    use std::net::{Ipv4Addr, Ipv6Addr};

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
    fn registry_unions_addresses_across_separate_resolutions() {
        // mdns-sd commonly delivers a peer's AAAA and A in separate events; the registry
        // must keep both so the dialer doesn't flap families and time out.
        let id = DeviceIdentity::generate();
        let reg = PeerRegistry::new();

        let mut v6_only = local_advert(&id, "MINIS", 60040, 0);
        v6_only.addrs = vec![IpAddr::V6(Ipv6Addr::new(0x2a00, 0, 0, 0, 0, 0, 0, 1))];
        reg.apply(PeerEvent::Found(v6_only));

        let mut v4_only = local_advert(&id, "MINIS", 60040, 0);
        v4_only.addrs = vec![IpAddr::V4(Ipv4Addr::new(192, 168, 1, 203))];
        reg.apply(PeerEvent::Found(v4_only));

        let merged = reg.find(&id.device_id()).expect("peer present");
        assert!(
            merged
                .addrs
                .contains(&IpAddr::V4(Ipv4Addr::new(192, 168, 1, 203)))
                && merged
                    .addrs
                    .contains(&IpAddr::V6(Ipv6Addr::new(0x2a00, 0, 0, 0, 0, 0, 0, 1))),
            "both families retained, got {:?}",
            merged.addrs,
        );
        // IPv6 resolved last, but the dial still prefers the routable IPv4.
        assert_eq!(
            peer_socket_addr(&merged),
            Some(SocketAddr::from(([192, 168, 1, 203], 60040))),
        );
    }

    #[test]
    fn peer_socket_addrs_returns_all_routable_ipv4_first() {
        // The multi-address dialer must get EVERY routable address (so a wrong family is
        // recovered by trying the other), ordered IPv4-first, with bare link-local IPv6
        // dropped (unreachable without a scope id — it would only burn a timeout).
        let id = DeviceIdentity::generate();
        let mut advert = local_advert(&id, "MINIS", 60040, 0);
        advert.addrs = vec![
            IpAddr::V6(Ipv6Addr::new(0xfe80, 0, 0, 0, 0, 0, 0, 1)), // link-local → dropped
            IpAddr::V6(Ipv6Addr::new(0x2a00, 0, 0, 0, 0, 0, 0, 1)), // routable v6
            IpAddr::V4(Ipv4Addr::new(192, 168, 1, 203)),            // routable v4
        ];
        assert_eq!(
            peer_socket_addrs(&advert),
            vec![
                SocketAddr::from(([192, 168, 1, 203], 60040)),
                SocketAddr::from((Ipv6Addr::new(0x2a00, 0, 0, 0, 0, 0, 0, 1), 60040)),
            ],
        );
        // No interactive port → nothing dialable.
        advert.iport = 0;
        assert!(peer_socket_addrs(&advert).is_empty());
    }

    #[test]
    fn peer_device_id_reads_the_advert_id() {
        let identity = DeviceIdentity::generate();
        let advert = local_advert(&identity, "Peer", 1, 2);
        assert_eq!(peer_device_id(&advert), Some(identity.device_id()));
    }

    #[test]
    fn local_ipv4_cache_reuses_value_until_ttl_expires() {
        use std::cell::Cell;

        let cache = LocalIpv4Cache::default();
        let start = Instant::now();
        let calls = Cell::new(0);
        let first_ip = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 10));
        let second_ip = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 11));

        let first = cache.get_or_probe(start, || {
            calls.set(calls.get() + 1);
            Some(first_ip)
        });
        let cached = cache.get_or_probe(start + Duration::from_secs(1), || {
            calls.set(calls.get() + 1);
            Some(second_ip)
        });
        let refreshed = cache.get_or_probe(start + LOCAL_IPV4_TTL, || {
            calls.set(calls.get() + 1);
            Some(second_ip)
        });

        assert_eq!(first, Some(first_ip));
        assert_eq!(cached, Some(first_ip));
        assert_eq!(refreshed, Some(second_ip));
        assert_eq!(calls.get(), 2);
    }
}
