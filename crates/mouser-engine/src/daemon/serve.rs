//! The serve roles (`auto`/`source`/`target`): advertise + discover over mDNS, run the
//! [`IpcBridge`], form one peer connection (auto-dial, accept, or an IPC `Connect`), and
//! run the session until ctrl-c or an IPC `Disconnect`.

use std::collections::BTreeSet;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use mouser_core::platform::{Clipboard, InputCapture, InputInjection};
use mouser_core::DeviceId;
use mouser_net::{
    DeviceIdentity, Discovery, InteractiveConnection, InteractiveEndpoint, PinPolicy,
};

use crate::daemon_store::{format_device_id, DaemonStore};
use crate::discovery::PeerRegistry;
use crate::{discovery, EngineCore, RuntimeHandle};

use super::clipboard::{self as clipboard_driver, SettingsProvider};
use super::ipc_bridge::{ConnectRequest, IpcBridge};
use super::pairing;
use super::reconnect::{redial_until_reconnected, ReconnectEnd};
use super::{hostname, source_layout, windows_firewall_hint};

struct SessionContext<'a> {
    store: &'a DaemonStore,
    registry: &'a PeerRegistry,
    bridge: Option<&'a IpcBridge>,
}

struct SessionAdapters {
    injector: Arc<dyn InputInjection>,
    capture: Arc<dyn InputCapture>,
    clipboard: Arc<dyn Clipboard>,
}

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
    let my_id = me.device_id();
    let my_b32 = me.device_id_base32();
    eprintln!("mouserd: device_id {my_b32}");
    eprintln!("mouserd: role {role}");

    // One endpoint both accepts (TrustOnFirstUse - trust is the §3 cert pin checked
    // against the mDNS-advertised id) and dials. Bind the dual-stack wildcard ([::]:0)
    // so the single listener accepts both IPv4 and IPv6 dialers (a peer may resolve us
    // to either family).
    let bind = mouser_net::dual_stack_addr();
    let endpoint = InteractiveEndpoint::bind_server(&me, bind, PinPolicy::TrustOnFirstUse)
        .map_err(|e| e.to_string())?;
    let iport = endpoint.local_addr().map_err(|e| e.to_string())?.port();
    let bulk_endpoint = Arc::new(
        mouser_net::BulkEndpoint::bind_server(&me, bind, PinPolicy::TrustOnFirstUse)
            .map_err(|e| e.to_string())?,
    );
    let bport = bulk_endpoint
        .local_addr()
        .map_err(|e| e.to_string())?
        .port();
    let bulk_task = tokio::spawn(run_bulk_accept_skeleton(Arc::clone(&bulk_endpoint)));

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
    eprintln!(
        "mouserd: advertising {}:{iport} bulk:{bport} as {}",
        if host_ip.is_empty() { "auto" } else { &host_ip },
        advert.instance_name()
    );
    windows_firewall_hint(iport);

    let registry = PeerRegistry::new();
    let browser = mdns.browse().map_err(|e| e.to_string())?;
    tokio::spawn(discovery::run_registry(browser, registry.clone()));
    eprintln!("mouserd: searching for peers on the local network...");

    // Bring up the local IPC link so the desktop UI can see/drive the engine. The bridge
    // reads the shared registry for its snapshots; failure to bind it is non-fatal (the
    // daemon still runs headless), so we warn and carry on.
    let bridge =
        match IpcBridge::start(store.clone(), my_b32.clone(), hostname(), registry.clone()).await {
            Ok(bridge) => Some(bridge),
            Err(e) => {
                eprintln!("mouserd: IPC unavailable ({e}); running headless");
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
        if let Some(bridge) = bridge.as_ref() {
            bridge.set_connected(&format_device_id(&peer), &my_b32);
        }
        eprintln!(
            "mouserd: connected; {}",
            if can_control {
                "this machine can control the peer"
            } else {
                "receive-only target mode"
            }
        );

        let end = run_session(
            my_id,
            peer,
            can_control,
            conn,
            SessionContext {
                store,
                registry: &registry,
                bridge: bridge.as_ref(),
            },
            SessionAdapters {
                injector: Arc::clone(&injector),
                capture: Arc::clone(&capture),
                clipboard: Arc::clone(&clipboard),
            },
        )
        .await;
        if let Some(bridge) = bridge.as_ref() {
            bridge.set_idle();
        }
        match end {
            SessionEnd::Shutdown => break,
            SessionEnd::Disconnected => {
                eprintln!("mouserd: session ended; searching for peers");
            }
            SessionEnd::ConnectionLost => {
                if role == "target" || !can_control {
                    eprintln!("mouserd: connection lost; returning to discovery");
                    continue;
                }
                eprintln!("mouserd: connection lost; redialing {peer:?}");
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
                        pending = Some(conn);
                    }
                    ReconnectEnd::Disconnected => {
                        eprintln!("mouserd: reconnect stopped; searching for peers");
                    }
                    ReconnectEnd::Shutdown => break,
                }
            }
        }
    }
    eprintln!("mouserd: shutting down");
    bulk_endpoint.close();
    bulk_task.abort();
    let _ = capture.stop();
    Ok(())
}

async fn run_bulk_accept_skeleton(endpoint: Arc<mouser_net::BulkEndpoint>) {
    loop {
        match endpoint.accept_bulk(0).await {
            Ok(conn) => {
                eprintln!("mouserd: bulk connection accepted; file receiver is not wired yet");
                conn.close();
            }
            Err(e) => {
                eprintln!("mouserd: bulk accept skipped: {e}");
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
        }
    }
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
                            eprintln!("mouserd: IPC connect failed: {e}");
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
                    Err(e) => { eprintln!("mouserd: accept error: {e}"); continue; }
                }
            }
            dialed = dial_discovered(store, endpoint, me, my_id, my_b32, registry, role == "source"), if can_dial => {
                match dialed {
                    Ok(conn) => return Some((conn, true)),
                    Err(e) => { eprintln!("mouserd: dial error: {e}"); continue; }
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
    let registry_addr = registry_addr_for(registry, &peer_id).await.ok_or_else(|| {
        format!(
            "peer {} not currently discoverable",
            format_device_id(&peer_id)
        )
    })?;
    let addr = request
        .addr
        .filter(|addr| *addr == registry_addr)
        .unwrap_or(registry_addr);
    eprintln!("mouserd: dialing {addr} (IPC connect)");
    endpoint
        .connect_interactive(me, addr, PinPolicy::Pinned(peer_id))
        .await
        .map_err(|e| e.to_string())
}

/// Wait up to 5s for `peer_id`'s current socket address to appear in the shared
/// discovery registry (used by an IPC dial), re-checking on each registry change.
async fn registry_addr_for(registry: &PeerRegistry, peer_id: &DeviceId) -> Option<SocketAddr> {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    let mut changes = registry.subscribe();
    loop {
        if let Some(addr) = registry
            .find(peer_id)
            .and_then(|p| discovery::peer_socket_addr(&p))
        {
            return Some(addr);
        }
        let remaining = deadline.checked_duration_since(tokio::time::Instant::now())?;
        if tokio::time::timeout(remaining, changes.changed())
            .await
            .is_err()
        {
            return None; // timed out before the peer became discoverable
        }
    }
}

async fn run_session(
    my_id: DeviceId,
    peer: DeviceId,
    can_control: bool,
    conn: InteractiveConnection,
    context: SessionContext<'_>,
    adapters: SessionAdapters,
) -> SessionEnd {
    let core = if can_control {
        EngineCore::new_source(my_id, peer, source_layout())
    } else {
        EngineCore::new_target(my_id, peer)
    };
    let mut runtime =
        RuntimeHandle::start(core, Arc::new(conn), adapters.injector, adapters.capture);
    let peer_os = context
        .registry
        .find(&peer)
        .map(|advert| clipboard_driver::os_from_str(&advert.os))
        .unwrap_or(mouser_protocol::Os::Unknown);
    let settings = context
        .bridge
        .map(|bridge| SettingsProvider::Bridge(bridge.settings_source()))
        .unwrap_or_else(|| SettingsProvider::Fixed(context.store.load_settings()));
    let clipboard_task = runtime.take_control_lane().map(|lane| {
        tokio::spawn(clipboard_driver::run_driver(
            lane,
            adapters.clipboard,
            my_id,
            peer,
            peer_os,
            settings,
        ))
    });

    if can_control {
        eprintln!(
            "mouserd: passive edge sensing active - local keyboard/mouse stay native; \
             suppressing capture installs only while controlling the peer"
        );
    } else {
        eprintln!("mouserd: target ready - injecting input received from the source");
    }

    // End the session on ctrl-c or an IPC Disconnect.
    let end = tokio::select! {
        _ = tokio::signal::ctrl_c() => SessionEnd::Shutdown,
        _ = wait_for_disconnect(context.bridge) => {
            eprintln!("mouserd: disconnect requested over IPC");
            SessionEnd::Disconnected
        }
        _ = runtime.wait_dead() => SessionEnd::ConnectionLost,
    };
    // Tear down the runtime tasks and the capture adapter (drops any hooks/poll).
    if let Some(task) = clipboard_task {
        task.abort();
    }
    runtime.shutdown();
    end
}

enum SessionEnd {
    Shutdown,
    Disconnected,
    ConnectionLost,
}

/// Resolve when an IPC `Disconnect` command arrives (inert in headless mode).
async fn wait_for_disconnect(bridge: Option<&IpcBridge>) {
    match bridge {
        Some(bridge) => bridge.next_disconnect_request().await,
        None => std::future::pending().await,
    }
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
                        eprintln!(
                            "mouserd: found untrusted peer {}; run `mouserd trust {peer_text}` \
                             on this machine before connecting",
                            peer.instance_name()
                        );
                    }
                    continue;
                }
                Err(e) => {
                    if warned_untrusted.insert(peer_id) {
                        eprintln!(
                            "mouserd: trust check failed for {}: {e}",
                            peer.instance_name()
                        );
                    }
                    continue;
                }
            }
            let Some(addr) = discovery::peer_socket_addr(&peer) else {
                continue;
            };
            eprintln!("mouserd: dialing {} at {addr}", peer.instance_name());
            return endpoint
                .connect_interactive(me, addr, PinPolicy::Pinned(peer_id))
                .await
                .map_err(|e| e.to_string());
        }
        // Nothing to dial yet; wait for the registry to change, then re-scan.
        if changes.changed().await.is_err() {
            return Err("discovery registry closed".to_string());
        }
    }
}
