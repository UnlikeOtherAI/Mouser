//! mDNS/DNS-SD discovery (§4) over the [`mdns_sd`] crate. Advertises and browses the
//! `_mouser._udp.local` service. TXT is **advisory only** — trust is established in §5,
//! never from TXT. The typed TXT keys (§4) are: `txtvers`, `id`, `name`, `os`, `ver`,
//! `iport`, `bport`, `caps`, `role`.

use std::collections::HashMap;
use std::net::IpAddr;

use mdns_sd::{IfKind, Receiver, ServiceDaemon, ServiceEvent, ServiceInfo};

use crate::NetError;

/// Create a `ServiceDaemon`, optionally enabling the loopback interfaces (disabled by
/// default in `mdns-sd`). Loopback is needed for single-host discovery (e.g. tests).
///
/// IPv6 is disabled: hosts (especially macOS) expose many virtual IPv6 link-local
/// interfaces — `awdl0` (Apple Wireless Direct Link), `llw0`, and per-VPN `utunN`.
/// Browsing and answering across all of them drowns the real LAN multicast in
/// `mdns-sd`, so peers on the wire are never received and our own service never answers
/// remote resolve queries (verified: with IPv6 enabled a LAN peer is invisible;
/// disabled, it resolves immediately). IPv4 mDNS is sufficient for a local-network KVM.
fn new_daemon(loopback: bool) -> Result<ServiceDaemon, NetError> {
    let daemon = ServiceDaemon::new().map_err(|e| NetError::Discovery(e.to_string()))?;
    daemon
        .disable_interface(IfKind::IPv6)
        .map_err(|e| NetError::Discovery(e.to_string()))?;
    if loopback {
        daemon
            .enable_interface(IfKind::LoopbackV4)
            .map_err(|e| NetError::Discovery(e.to_string()))?;
    }
    Ok(daemon)
}

/// The Mouser DNS-SD service type (§4).
pub const SERVICE_TYPE: &str = "_mouser._udp.local.";

/// TXT version (§4: `txtvers=1`).
pub const TXT_VERSION: &str = "1";

/// The advertised attributes of a Mouser peer (§4). Mirrors the typed TXT keys.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PeerAdvert {
    /// `id`: base32 (no-pad, lowercase) of the full `device_id`.
    pub id: String,
    /// `name`: display name.
    pub name: String,
    /// `os`: OS string (e.g. `macos`).
    pub os: String,
    /// `ver`: engine version (display only).
    pub ver: String,
    /// `iport`: interactive-connection UDP port.
    pub iport: u16,
    /// `bport`: bulk-connection UDP port.
    pub bport: u16,
    /// `caps`: advisory capability CSV (untrusted hint).
    pub caps: String,
    /// `role`: coordinator-eligibility role string.
    pub role: String,
    /// Resolved IP address(es) of the peer (from mDNS A/AAAA records, C2-6). A peer with
    /// no resolved address can't be dialed, so [`PeerAdvert::from_service_info`] returns
    /// `None` for one; the connect helpers pair these with `iport`/`bport` for a
    /// `SocketAddr`. Not part of TXT — these are the SRV/address records, not advisory.
    pub addrs: Vec<IpAddr>,
}

impl PeerAdvert {
    /// The DNS-SD instance name (§4): `"<display name> (<short id>)"`, unique even
    /// when display names collide.
    pub fn instance_name(&self) -> String {
        let short = self.id.get(..8).unwrap_or(self.id.as_str());
        format!("{} ({})", self.name, short)
    }

    fn txt_map(&self) -> HashMap<String, String> {
        let mut txt = HashMap::new();
        txt.insert("txtvers".to_string(), TXT_VERSION.to_string());
        txt.insert("id".to_string(), self.id.clone());
        txt.insert("name".to_string(), self.name.clone());
        txt.insert("os".to_string(), self.os.clone());
        txt.insert("ver".to_string(), self.ver.clone());
        txt.insert("iport".to_string(), self.iport.to_string());
        txt.insert("bport".to_string(), self.bport.to_string());
        txt.insert("caps".to_string(), self.caps.clone());
        txt.insert("role".to_string(), self.role.clone());
        txt
    }

    /// Parse a [`PeerAdvert`] back from a resolved [`ServiceInfo`]'s TXT records,
    /// ignoring unknown keys (§4 forward-compat). Returns `None` when the service has no
    /// `id` TXT key **or no resolved address** (C2-6): an address-less peer cannot be
    /// dialed, so it is skipped rather than surfaced as an undialable [`PeerAdvert`].
    pub fn from_service_info(info: &ServiceInfo) -> Option<Self> {
        let get = |k: &str| info.get_property_val_str(k).map(str::to_string);
        let addrs: Vec<IpAddr> = info.get_addresses().iter().copied().collect();
        if addrs.is_empty() {
            return None;
        }
        Some(Self {
            id: get("id")?,
            name: get("name").unwrap_or_default(),
            os: get("os").unwrap_or_default(),
            ver: get("ver").unwrap_or_default(),
            iport: get("iport").and_then(|s| s.parse().ok()).unwrap_or(0),
            bport: get("bport").and_then(|s| s.parse().ok()).unwrap_or(0),
            caps: get("caps").unwrap_or_default(),
            role: get("role").unwrap_or_default(),
            addrs,
        })
    }
}

/// An event from a [`Browser`] (C2-6): a peer resolved (`Found`) or a peer that left the
/// network (`Removed`). `Removed` carries the DNS-SD instance fullname (the same string
/// the daemon emits in `ServiceRemoved`), which the reconnect supervisor matches against
/// a previously-`Found` peer to prune it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PeerEvent {
    /// A peer was fully resolved (TXT parsed, at least one dialable address).
    Found(PeerAdvert),
    /// A previously-advertised peer departed; the `String` is its instance fullname.
    Removed(String),
}

/// A running mDNS advertisement; dropping it (or calling [`Advertiser::unregister`])
/// stops the announcement.
pub struct Advertiser {
    daemon: ServiceDaemon,
    fullname: String,
}

impl Advertiser {
    /// Advertise `advert` on `host_ip:advert.iport` over mDNS (§4).
    pub fn advertise(advert: &PeerAdvert, host_ip: &str) -> Result<Self, NetError> {
        Self::advertise_with(advert, host_ip, false)
    }

    /// As [`Advertiser::advertise`], but also enable the loopback interface so the
    /// service is discoverable on the same host (e.g. `127.0.0.1`, tests).
    pub fn advertise_loopback(advert: &PeerAdvert) -> Result<Self, NetError> {
        Self::advertise_with(advert, "127.0.0.1", true)
    }

    fn advertise_with(
        advert: &PeerAdvert,
        host_ip: &str,
        loopback: bool,
    ) -> Result<Self, NetError> {
        let daemon = new_daemon(loopback)?;
        let (info, fullname) = build_service_info(advert, host_ip)?;
        daemon
            .register(info)
            .map_err(|e| NetError::Discovery(e.to_string()))?;
        Ok(Self { daemon, fullname })
    }

    /// Stop advertising this service.
    pub fn unregister(&self) -> Result<(), NetError> {
        self.daemon
            .unregister(&self.fullname)
            .map_err(|e| NetError::Discovery(e.to_string()))?;
        Ok(())
    }
}

impl Drop for Advertiser {
    fn drop(&mut self) {
        let _ = self.daemon.unregister(&self.fullname);
        let _ = self.daemon.shutdown();
    }
}

/// Build the `_mouser._udp` [`ServiceInfo`] for `advert` on `host_ip` (§4), returning
/// it alongside its DNS-SD fullname.
fn build_service_info(
    advert: &PeerAdvert,
    host_ip: &str,
) -> Result<(ServiceInfo, String), NetError> {
    let host_name = format!("{}.local.", advert.id);
    let info = ServiceInfo::new(
        SERVICE_TYPE,
        &advert.instance_name(),
        &host_name,
        host_ip,
        advert.iport,
        advert.txt_map(),
    )
    .map_err(|e| NetError::Discovery(e.to_string()))?;
    let fullname = info.get_fullname().to_string();
    Ok((info, fullname))
}

/// A single mDNS endpoint — ONE [`ServiceDaemon`] used for **both** advertising and
/// browsing. A host must share one of these: every `ServiceDaemon` binds the port-5353
/// multicast sockets, and several on one host race for inbound packets — on macOS all
/// but one silently miss remote peers. The advertisement lasts until the [`Discovery`]
/// is dropped, which shuts the daemon down (ending the browse too).
pub struct Discovery {
    daemon: ServiceDaemon,
}

impl Discovery {
    /// Create the shared endpoint on the IPv4 LAN interfaces (see [`new_daemon`]).
    pub fn new() -> Result<Self, NetError> {
        Ok(Self {
            daemon: new_daemon(false)?,
        })
    }

    /// As [`Discovery::new`] but with the loopback interface enabled (same-host tests).
    pub fn new_loopback() -> Result<Self, NetError> {
        Ok(Self {
            daemon: new_daemon(true)?,
        })
    }

    /// Advertise this host (§4). The registration lasts until `self` is dropped.
    pub fn advertise(&self, advert: &PeerAdvert, host_ip: &str) -> Result<(), NetError> {
        let (info, _fullname) = build_service_info(advert, host_ip)?;
        self.daemon
            .register(info)
            .map_err(|e| NetError::Discovery(e.to_string()))?;
        Ok(())
    }

    /// Start the single browse for `_mouser._udp` peers. Call once per endpoint:
    /// mdns-sd keys queriers by service type, so a second browse replaces the first's
    /// receiver. The returned [`Browser`] does not own the daemon (this does).
    pub fn browse(&self) -> Result<Browser, NetError> {
        let events = self
            .daemon
            .browse(SERVICE_TYPE)
            .map_err(|e| NetError::Discovery(e.to_string()))?;
        Ok(Browser {
            daemon: None,
            events,
        })
    }
}

impl Drop for Discovery {
    fn drop(&mut self) {
        let _ = self.daemon.shutdown();
    }
}

/// A browse session yielding resolved peers as they appear on the network (§4).
pub struct Browser {
    /// `Some` when this browser owns its daemon (standalone) and shuts it down on drop;
    /// `None` when the daemon is owned by a shared [`Discovery`].
    daemon: Option<ServiceDaemon>,
    events: Receiver<ServiceEvent>,
}

impl Browser {
    /// Start browsing for `_mouser._udp.local` peers.
    pub fn browse() -> Result<Self, NetError> {
        Self::browse_with(false)
    }

    /// As [`Browser::browse`], but also enable the loopback interface so same-host
    /// services are discovered (tests).
    pub fn browse_loopback() -> Result<Self, NetError> {
        Self::browse_with(true)
    }

    fn browse_with(loopback: bool) -> Result<Self, NetError> {
        let daemon = new_daemon(loopback)?;
        let events = daemon
            .browse(SERVICE_TYPE)
            .map_err(|e| NetError::Discovery(e.to_string()))?;
        Ok(Self {
            daemon: Some(daemon),
            events,
        })
    }

    /// Await the next [`PeerEvent`] (C2-6): a resolved, dialable peer
    /// ([`PeerEvent::Found`]) or a departure ([`PeerEvent::Removed`]), skipping the
    /// daemon's intermediate browse events (search-started, service-found-but-unresolved,
    /// …). A `ServiceResolved` whose TXT lacks `id` or whose address set is empty is
    /// skipped (can't be dialed). Returns `None` if the browse channel closes.
    pub async fn next_event(&self) -> Option<PeerEvent> {
        loop {
            match self.events.recv_async().await.ok()? {
                ServiceEvent::ServiceResolved(info) => {
                    if let Some(peer) = PeerAdvert::from_service_info(&info) {
                        return Some(PeerEvent::Found(peer));
                    }
                }
                ServiceEvent::ServiceRemoved(_service_type, fullname) => {
                    return Some(PeerEvent::Removed(fullname));
                }
                _ => continue,
            }
        }
    }
}

impl Drop for Browser {
    fn drop(&mut self) {
        // Only a standalone browser owns its daemon; a shared [`Discovery`] shuts its
        // own daemon down.
        if let Some(daemon) = &self.daemon {
            let _ = daemon.shutdown();
        }
    }
}
