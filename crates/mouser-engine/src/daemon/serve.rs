//! The serve roles (`auto`/`source`/`target`): advertise + discover over mDNS, run the
//! [`IpcBridge`], form one peer connection (auto-dial, accept, or an IPC `Connect`), and
//! run the session until ctrl-c or an IPC `Disconnect`.

use std::collections::BTreeSet;
use std::net::SocketAddr;
use std::sync::Arc;

use mouser_core::platform::{Clipboard, InputCapture, InputInjection};
use mouser_core::DeviceId;
use mouser_net::{
    DeviceIdentity, Discovery, InteractiveConnection, InteractiveEndpoint, PinPolicy,
};

use crate::daemon_store::{format_device_id, DaemonStore};
use crate::discovery;
use crate::discovery::PeerRegistry;

use super::file_transfer::{self, ActiveBulkSession};
use super::ipc_bridge::{ConnectRequest, IpcBridge};
use super::pairing;
use super::reconnect::{redial_until_reconnected, ReconnectEnd};
use super::serve_session::{run_session, SessionAdapters, SessionContext, SessionEnd};
use super::{hostname, windows_firewall_hint};

/// A serve role (`auto`/`source`/`target`): advertise + discover over mDNS, run the
/// [`IpcBridge`] so the desktop UI reflects/drives the engine, then form one peer
/// connection (auto-discovered, accepted, or an IPC `Connect`) and run it until
/// ctrl-c or an IPC `Disconnect`. Single-session v1; the IPC link is the control
/// surface on top.
pub(super) async fn serve(
    store: &DaemonStore,
    role: &str,
    injector: Arc<dyn InputInjection>,
    capture: Arc<dyn InputCapture>,
    clipboard: Arc<dyn Clipboard>,
) -> Result<(), String> {
    let me = store.load_or_create_identity().map_err(|e| e.to_string())?;
    let me = Arc::new(me);
    let my_id = me.device_id();
    let my_b32 = me.device_id_base32();
    crate::diag!(info, "mouserd: device_id {my_b32}");
    crate::diag!(info, "mouserd: role {role}");

    // One endpoint both accepts (TrustOnFirstUse - trust is the §3 cert pin checked
    // against the mDNS-advertised id) and dials. Bind the dual-stack wildcard ([::]:0)
    // so the single listener accepts both IPv4 and IPv6 dialers (a peer may resolve us
    // to either family). On a host with IPv6 disabled the [::] bind fails, so fall back
    // to the IPv4 wildcard rather than refusing to start (IPv4 LAN connectivity still
    // works; only inbound IPv6 peers, e.g. an iPhone over v6-only, are lost).
    let bind = mouser_net::dual_stack_addr();
    let v4_bind = SocketAddr::from(([0u8, 0, 0, 0], 0));
    let endpoint = InteractiveEndpoint::bind_server(&me, bind, PinPolicy::TrustOnFirstUse)
        .or_else(|e| {
            crate::diag!(
                info,
                "mouserd: dual-stack bind failed ({e}); falling back to IPv4-only"
            );
            InteractiveEndpoint::bind_server(&me, v4_bind, PinPolicy::TrustOnFirstUse)
        })
        .map_err(|e| e.to_string())?;
    let iport = endpoint.local_addr().map_err(|e| e.to_string())?.port();
    let bulk_endpoint = Arc::new(
        mouser_net::BulkEndpoint::bind_server(&me, bind, PinPolicy::TrustOnFirstUse)
            .or_else(|_| {
                mouser_net::BulkEndpoint::bind_server(&me, v4_bind, PinPolicy::TrustOnFirstUse)
            })
            .map_err(|e| e.to_string())?,
    );
    let bport = bulk_endpoint
        .local_addr()
        .map_err(|e| e.to_string())?
        .port();
    let (bulk_session_tx, bulk_session_rx) = tokio::sync::watch::channel(None);
    let (clipboard_bulk_tx, clipboard_bulk_rx) = super::clipboard_bulk::channel();
    let bulk_task = tokio::spawn(file_transfer::run_bulk_acceptor(
        Arc::clone(&bulk_endpoint),
        bulk_session_rx,
        file_transfer::quarantine_dir(store),
        clipboard_bulk_tx.clone(),
    ));

    // One shared mDNS endpoint advertises this host (§4) and feeds a single browse into
    // one peer registry that both the dialer and the IPC snapshot read. A host must use
    // ONE mDNS daemon: several browsing daemons race for inbound multicast and silently
    // drop peers on macOS. Kept alive for the whole serve() (drop ends advertise+browse).
    // Initial address hint; empty when there's no route yet. The advertiser uses
    // auto-addr, so it fills/updates the A records from the host's interfaces and never
    // pins a stale or unspecified (0.0.0.0) address.
    let host_ip = discovery::local_ipv4()
        .map(|ip| ip.to_string())
        .unwrap_or_default();
    let advert = discovery::local_advert(&me, &hostname(), iport, bport);
    let mdns = Discovery::new().map_err(|e| e.to_string())?;
    mdns.advertise(&advert, &host_ip)
        .map_err(|e| e.to_string())?;
    crate::diag!(
        info,
        "mouserd: advertising {}:{iport} bulk:{bport} as {}",
        if host_ip.is_empty() { "auto" } else { &host_ip },
        advert.instance_name()
    );
    windows_firewall_hint(iport);

    let registry = PeerRegistry::new();
    let browser = mdns.browse().map_err(|e| e.to_string())?;
    tokio::spawn(discovery::run_registry(browser, registry.clone()));
    crate::diag!(info, "mouserd: searching for peers on the local network...");

    // Bring up the local IPC link so the desktop UI can see/drive the engine. The bridge
    // reads the shared registry for its snapshots; failure to bind it is non-fatal (the
    // daemon still runs headless), so we warn and carry on.
    let bridge =
        match IpcBridge::start(store.clone(), my_b32.clone(), hostname(), registry.clone()).await {
            Ok(bridge) => Some(bridge),
            Err(e) => {
                crate::diag!(info, "mouserd: IPC unavailable ({e}); running headless");
                None
            }
        };

    let mut pending = None;
    loop {
        let (conn, can_control) = if let Some(conn) = pending.take() {
            (conn, true)
        } else {
            let Some(found) = next_connection(
                store,
                &endpoint,
                &me,
                my_id,
                &my_b32,
                &registry,
                role,
                bridge.as_ref(),
            )
            .await
            else {
                break;
            };
            found
        };

        let peer = conn
            .peer_device_id()
            .ok_or("peer did not present a device_id")?;
        let peer_text = format_device_id(&peer);
        let session_id = conn.session_id();
        let peer_session_id = conn.peer_session_id();
        if let Some(bridge) = bridge.as_ref() {
            bridge.set_connected(&peer_text, &my_b32);
        }
        crate::diag!(
            info,
            "mouserd: connected peer_id={peer_text} session_id={session_id} peer_session_id={peer_session_id}; {}",
            if can_control {
                "this machine can control the peer"
            } else {
                "receive-only target mode"
            }
        );
        bulk_session_tx.send_replace(Some(ActiveBulkSession {
            peer_id: peer,
            expected_session_id: peer_session_id,
        }));

        let end = run_session(
            my_id,
            peer,
            can_control,
            conn,
            SessionContext {
                store,
                registry: &registry,
                bridge: bridge.as_ref(),
                identity: Arc::clone(&me),
                bulk_endpoint: Arc::clone(&bulk_endpoint),
                bulk_session_id: session_id,
                clipboard_bulk_rx: Arc::clone(&clipboard_bulk_rx),
            },
            SessionAdapters {
                injector: Arc::clone(&injector),
                capture: Arc::clone(&capture),
                clipboard: Arc::clone(&clipboard),
            },
        )
        .await;
        bulk_session_tx.send_replace(None);
        match end {
            SessionEnd::Shutdown => {
                if let Some(bridge) = bridge.as_ref() {
                    bridge.set_idle();
                }
                break;
            }
            SessionEnd::Disconnected => {
                if let Some(bridge) = bridge.as_ref() {
                    bridge.set_idle();
                }
                crate::diag!(info, "mouserd: session ended; searching for peers");
            }
            SessionEnd::ConnectionLost { reason } => {
                if let Some(bridge) = bridge.as_ref() {
                    bridge.set_connect_error(&reason);
                }
                if role == "target" || !can_control {
                    crate::diag!(
                        warn,
                        "mouserd: connection lost ({reason}); returning to discovery"
                    );
                    continue;
                }
                crate::diag!(warn, "mouserd: connection lost ({reason}); redialing");
                match redial_until_reconnected(
                    store,
                    &endpoint,
                    &me,
                    &registry,
                    peer,
                    bridge.as_ref(),
                )
                .await
                {
                    ReconnectEnd::Reconnected(conn) => {
                        pending = Some(*conn);
                    }
                    ReconnectEnd::Disconnected => {
                        crate::diag!(info, "mouserd: reconnect stopped; searching for peers");
                    }
                    ReconnectEnd::Shutdown => break,
                }
            }
        }
    }
    crate::diag!(info, "mouserd: shutting down");
    bulk_endpoint.close();
    bulk_task.abort();
    let _ = capture.stop();
    Ok(())
}

/// Wait for the connection to form: an IPC `Connect{peer_id}` to a trusted,
/// discovered peer, an auto-discovered dial (auto/source), or an inbound accept.
/// Returns `(connection, can_control)`, or `None` if ctrl-c fired first.
#[allow(clippy::too_many_arguments)]
async fn next_connection(
    store: &DaemonStore,
    endpoint: &InteractiveEndpoint,
    me: &DeviceIdentity,
    my_id: DeviceId,
    my_b32: &str,
    registry: &PeerRegistry,
    role: &str,
    bridge: Option<&IpcBridge>,
) -> Option<(InteractiveConnection, bool)> {
    // `target` only accepts; `source`/`auto` may dial. Either way an IPC Connect can
    // explicitly drive a dial to a chosen trusted peer.
    let can_dial = role != "target";
    // Consecutive auto-dial failures, so a reachable-but-failing peer (mid-restart, cert
    // mismatch, firewall) is re-dialed with a capped backoff instead of being hammered
    // back-to-back. The reconnect supervisor already does this post-session; this is the
    // initial-dial equivalent.
    let mut dial_failures = 0u32;
    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                return None;
            }
            ipc = wait_for_connect(bridge) => {
                if let Some(request) = ipc {
                    let peer_text = format_device_id(&request.peer_id);
                    if let Some(bridge) = bridge { bridge.set_connecting(&peer_text); }
                    match dial_connect_request(store, endpoint, me, registry, request).await {
                        Ok(conn) => return Some((conn, true)),
                        Err(e) => {
                            crate::diag!(info, "mouserd: IPC connect failed: {e}");
                            // Surface the reason to the UI instead of silently idling.
                            if let Some(bridge) = bridge { bridge.set_connect_error(&e); }
                            continue;
                        }
                    }
                }
            }
            accepted = pairing::accept_trusted(endpoint, store, registry, bridge) => {
                match accepted {
                    Ok(conn) => return Some((conn, false)),
                    Err(e) => { crate::diag!(info, "mouserd: accept error: {e}"); continue; }
                }
            }
            dialed = dial_discovered_backoff(store, endpoint, me, my_id, my_b32, registry, role == "source", dial_failures), if can_dial => {
                match dialed {
                    // On success we return out of next_connection; dial_failures is local
                    // to this call, so the next call starts fresh at 0 — no reset needed.
                    Ok(conn) => return Some((conn, true)),
                    Err(e) => {
                        crate::diag!(info, "mouserd: dial error: {e}");
                        dial_failures = dial_failures.saturating_add(1);
                        continue;
                    }
                }
            }
        }
    }
}

/// Resolve an IPC `Connect` command into the next request, or never resolve when
/// there is no IPC bridge (so the `select!` arm is inert in headless mode).
async fn wait_for_connect(bridge: Option<&IpcBridge>) -> Option<ConnectRequest> {
    match bridge {
        Some(bridge) => bridge.next_connect_request().await,
        None => std::future::pending().await,
    }
}

/// Dial a specific trusted peer chosen over IPC. Prefer the address the desktop already
/// resolved over its own browse (if supplied); otherwise resolve it from this daemon's
/// live discovery registry. Errors if the peer is untrusted or not discoverable.
async fn dial_connect_request(
    store: &DaemonStore,
    endpoint: &InteractiveEndpoint,
    me: &DeviceIdentity,
    registry: &PeerRegistry,
    request: ConnectRequest,
) -> Result<InteractiveConnection, String> {
    let peer_id = request.peer_id;
    if !store.is_peer_trusted(&peer_id).map_err(|e| e.to_string())? {
        return Err(format!(
            "peer {} is not trusted on this machine",
            format_device_id(&peer_id)
        ));
    }
    // Dial every address the registry resolved for the peer, IPv4-first, so one wrong
    // family doesn't sink the connect (the §3 cert pin is the only trust anchor).
    let addrs = registry_addrs_for(registry, &peer_id).await;
    if addrs.is_empty() {
        return Err(format!(
            "peer {} not currently discoverable",
            format_device_id(&peer_id)
        ));
    }
    crate::diag!(
        info,
        "mouserd: dialing {} ({} candidate address(es), IPC connect)",
        format_device_id(&peer_id),
        addrs.len()
    );
    endpoint
        .connect_interactive_any(me, &addrs, PinPolicy::Pinned(peer_id))
        .await
        .map_err(|e| e.to_string())
}

/// Wait up to 5s for `peer_id`'s current socket addresses to appear in the shared
/// discovery registry (used by an IPC dial), re-checking on each registry change.
/// Returns every dialable address (IPv4-first), or empty if none resolve in time.
async fn registry_addrs_for(registry: &PeerRegistry, peer_id: &DeviceId) -> Vec<SocketAddr> {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    let mut changes = registry.subscribe();
    loop {
        let addrs = registry
            .find(peer_id)
            .map(|p| discovery::peer_socket_addrs(&p))
            .unwrap_or_default();
        if !addrs.is_empty() {
            return addrs;
        }
        let Some(remaining) = deadline.checked_duration_since(tokio::time::Instant::now()) else {
            return Vec::new(); // deadline passed
        };
        if tokio::time::timeout(remaining, changes.changed())
            .await
            .is_err()
        {
            return Vec::new(); // timed out before the peer became discoverable
        }
    }
}

/// [`dial_discovered`] plus a capped backoff on repeated failure. The backoff sleeps
/// **inside** this future (one arm of [`next_connection`]'s `select!`), so the accept and
/// IPC arms stay responsive while we wait — a peer reconnecting to us is still picked up.
/// `failures` is the count of consecutive prior failures (0 ⇒ retry immediately).
#[allow(clippy::too_many_arguments)]
async fn dial_discovered_backoff(
    store: &DaemonStore,
    endpoint: &InteractiveEndpoint,
    me: &DeviceIdentity,
    my_id: DeviceId,
    my_b32: &str,
    registry: &PeerRegistry,
    force: bool,
    failures: u32,
) -> Result<InteractiveConnection, String> {
    let result = dial_discovered(store, endpoint, me, my_id, my_b32, registry, force).await;
    if result.is_err() && failures > 0 {
        tokio::time::sleep(super::reconnect::reconnect_backoff(failures)).await;
    }
    result
}

/// Browse mDNS until a dialable peer appears and dial it (device_id-pinned, §3).
/// When `force` is false (auto mode) only the lower `device_id` dials, so the two
/// sides don't connect twice.
async fn dial_discovered(
    store: &DaemonStore,
    endpoint: &InteractiveEndpoint,
    me: &DeviceIdentity,
    my_id: DeviceId,
    my_b32: &str,
    registry: &PeerRegistry,
    force: bool,
) -> Result<InteractiveConnection, String> {
    let mut warned_untrusted = BTreeSet::new();
    let mut changes = registry.subscribe();
    loop {
        // Scan the current registry for a trusted, dialable peer we should dial.
        for peer in registry.peers() {
            if peer.id == my_b32 {
                continue; // never dial ourselves
            }
            let Some(peer_id) = discovery::peer_device_id(&peer) else {
                continue;
            };
            if !force && my_id >= peer_id {
                continue; // the peer (lower id) will dial us; we accept instead
            }
            // Skip-and-warn-once on both untrusted and trust-check errors. A propagating
            // `?` here would bubble out of the function; `next_connection` re-enters this
            // auto-dial arm immediately (no await), so a persistently failing trust check
            // (e.g. an unreadable trusted-peers file) would spin the CPU.
            match store.is_peer_trusted(&peer_id) {
                Ok(true) => {}
                Ok(false) => {
                    if warned_untrusted.insert(peer_id) {
                        let peer_text = format_device_id(&peer_id);
                        crate::diag!(
                            info,
                            "mouserd: found untrusted peer {}; run `mouserd trust {peer_text}` \
                             on this machine before connecting",
                            peer.instance_name()
                        );
                    }
                    continue;
                }
                Err(e) => {
                    if warned_untrusted.insert(peer_id) {
                        crate::diag!(
                            info,
                            "mouserd: trust check failed for {}: {e}",
                            peer.instance_name()
                        );
                    }
                    continue;
                }
            }
            let addrs = discovery::peer_socket_addrs(&peer);
            if addrs.is_empty() {
                continue;
            }
            crate::diag!(
                info,
                "mouserd: dialing {} ({} candidate address(es))",
                peer.instance_name(),
                addrs.len()
            );
            return endpoint
                .connect_interactive_any(me, &addrs, PinPolicy::Pinned(peer_id))
                .await
                .map_err(|e| e.to_string());
        }
        // Nothing to dial yet; wait for the registry to change, then re-scan.
        if changes.changed().await.is_err() {
            return Err("discovery registry closed".to_string());
        }
    }
}
