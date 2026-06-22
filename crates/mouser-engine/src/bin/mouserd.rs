//! `mouserd` - the Mouser engine daemon.
//!
//! v1 single-peer bring-up: **auto-discover a peer over mDNS** (`_mouser._udp.local`,
//! §4), establish the device_id-pinned interactive QUIC connection (§3), and run the
//! [`mouser_engine`] runtime wired to the host's real capture/injection adapters:
//! - macOS -> `platform-mac` (`MacCapture` + `MacInjector`),
//! - Windows -> `platform-win` (`WinCapture` + `WinInjector`),
//! - Linux -> `platform-linux` (`LinuxCapture` + `UinputInjector`).
//!
//! Usage:
//! - `mouserd`          - auto on macOS/Linux; receive-only target mode on Windows.
//! - `mouserd auto`     - advertise + browse; either connected side can control.
//! - `mouserd source`   - controller-only connection: capture + dial a discovered peer.
//! - `mouserd target`   - receive-only connection: accept + inject, no input hooks.
//! - `mouserd connect <host:port> <peer-id>` - direct trusted controller connection.
//! - `mouserd probe <host:port>`   - handshake-only transport check, no capture/inject.
//! - `mouserd identity` - print this machine's persistent device id.
//! - `mouserd trust <peer-id>` / `mouserd trusted` - manage trusted peer pins.
//!
//! While in a serve role (`auto`/`source`/`target`) the daemon also runs the
//! [`mouser_ipc`] **Server** on a local Unix-domain socket so the Tauri desktop UI can
//! reflect the engine's live state (discovered peers + trust + connection) and drive it
//! (`Connect{peer_id}` / `Disconnect`). The IPC server is gated to serve roles: the
//! `probe`/`connect`/`identity`/`trust` local commands never start it.
//!
//! Discovery itself is platform-agnostic (it lives in the shared `mouser-engine`/
//! `mouser-net` crates over `mdns-sd`), so the same serve loop runs on every host;
//! only the concrete capture/injection adapters differ per OS (selected in `main`).
//! The §5 SAS pairing UI and CRDT layout sit above.

fn main() {
    #[cfg(target_os = "macos")]
    {
        let injector = std::sync::Arc::new(platform_mac::adapter::MacInjector::new());
        let capture = platform_mac::adapter::MacCapture::new();
        engine::run(injector, Box::new(capture));
    }
    #[cfg(target_os = "linux")]
    {
        let injector = match platform_linux::UinputInjector::new() {
            Ok(inj) => std::sync::Arc::new(inj),
            Err(e) => {
                eprintln!(
                    "mouserd: cannot open /dev/uinput ({e}); add the user to the \
                     `input` group (or run as root) and relaunch"
                );
                std::process::exit(1);
            }
        };
        let capture = platform_linux::LinuxCapture::new();
        engine::run(injector, Box::new(capture));
    }
    #[cfg(target_os = "windows")]
    {
        let injector = std::sync::Arc::new(platform_win::WinInjector::new());
        let capture = platform_win::WinCapture::new();
        engine::run(injector, Box::new(capture));
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        eprintln!(
            "mouserd: this host's platform adapters are not wired into the daemon yet \
             (macOS, Windows and Linux are supported). The engine library is platform-agnostic."
        );
        std::process::exit(1);
    }
}

/// Platform-agnostic daemon flow, parameterized over the host's concrete
/// capture/injection adapters (the only per-OS difference).
#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
mod engine {
    use std::collections::BTreeSet;
    use std::net::SocketAddr;
    use std::sync::Arc;

    use mouser_core::platform::{
        CaptureDecision as CoreDecision, InputCapture, InputInjection, InputSink, LocalInputEvent,
    };
    use mouser_core::DeviceId;
    use mouser_engine::core::CaptureDecision;
    use mouser_engine::daemon_store::{format_device_id, parse_peer_id_arg, DaemonStore};
    use mouser_engine::{discovery, EdgeLayout, EngineCore, RuntimeHandle};
    use mouser_net::{
        Advertiser, Browser, DeviceIdentity, InteractiveConnection, InteractiveEndpoint, PeerEvent,
        PinPolicy,
    };

    use crate::ipc_bridge::IpcBridge;

    /// Bridges the platform capture sink to the engine runtime: every local event is
    /// fed to the core, which decides suppress vs pass-through.
    struct EngineSink {
        runtime: Arc<RuntimeHandle>,
    }

    impl InputSink for EngineSink {
        fn on_event(&self, event: LocalInputEvent) -> CoreDecision {
            match self.runtime.feed_local(event) {
                CaptureDecision::Suppress => CoreDecision::Suppress,
                CaptureDecision::PassThrough => CoreDecision::PassThrough,
            }
        }
    }

    /// Run the daemon with the host's `injector` and `capture` adapters.
    pub fn run(injector: Arc<dyn InputInjection>, capture: Box<dyn InputCapture>) {
        let args: Vec<String> = std::env::args().collect();
        let arg1 = args
            .get(1)
            .cloned()
            .unwrap_or_else(|| default_role().to_string());
        let store = match DaemonStore::open_default() {
            Ok(store) => store,
            Err(e) => {
                eprintln!("mouserd: {e}");
                std::process::exit(1);
            }
        };

        match handle_local_command(&arg1, &args, &store) {
            Ok(true) => return,
            Ok(false) => {}
            Err(e) => {
                eprintln!("mouserd: {e}");
                std::process::exit(1);
            }
        }

        let rt = match tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
        {
            Ok(rt) => rt,
            Err(e) => {
                eprintln!("mouserd: failed to start tokio runtime: {e}");
                std::process::exit(1);
            }
        };

        // Direct modes take an explicit peer address and bypass mDNS (and IPC).
        if arg1 == "probe" || arg1 == "connect" {
            let Some(addr_str) = args.get(2).cloned() else {
                eprintln!(
                    "mouserd: `{arg1}` needs <host:port>, e.g. mouserd {arg1} 192.168.1.230:49970"
                );
                std::process::exit(1);
            };
            let addr: SocketAddr = match addr_str.parse() {
                Ok(a) => a,
                Err(e) => {
                    eprintln!("mouserd: bad address {addr_str}: {e}");
                    std::process::exit(1);
                }
            };
            rt.block_on(async move {
                let result = if arg1 == "probe" {
                    probe(&store, addr).await
                } else if let Some(peer_id_arg) = args.get(3).cloned() {
                    match parse_peer_id_arg(&peer_id_arg) {
                        Ok(peer_id) => serve_direct(&store, addr, peer_id, injector, capture).await,
                        Err(e) => Err(e.to_string()),
                    }
                } else {
                    Err(format!(
                        "`connect` needs a trusted <peer-id>. Run `mouserd probe {addr}` \
                         to read the peer id, then `mouserd trust <peer-id>`, then \
                         `mouserd connect {addr} <peer-id>`"
                    ))
                };
                if let Err(e) = result {
                    eprintln!("mouserd: {e}");
                    std::process::exit(1);
                }
            });
            return;
        }

        let role = role_from_arg(&arg1);
        rt.block_on(async move {
            if let Err(e) = serve(&store, &role, injector, capture).await {
                eprintln!("mouserd: {e}");
                std::process::exit(1);
            }
        });
    }

    fn handle_local_command(
        command: &str,
        args: &[String],
        store: &DaemonStore,
    ) -> Result<bool, String> {
        match command {
            "identity" | "id" => {
                let identity = store.load_or_create_identity().map_err(|e| e.to_string())?;
                println!("{}", identity.device_id_base32());
                println!("store {}", store.dir().display());
                Ok(true)
            }
            "trusted" => {
                for peer in store.trusted_peer_ids().map_err(|e| e.to_string())? {
                    println!("{}", format_device_id(&peer));
                }
                Ok(true)
            }
            "trust" => {
                let Some(peer_id) = args.get(2) else {
                    return Err("`trust` needs a <peer-id>, e.g. mouserd trust abc...".to_string());
                };
                let trusted = store
                    .trust_peer_base32(peer_id)
                    .map_err(|e| e.to_string())?;
                println!("trusted {}", format_device_id(&trusted));
                println!("store {}", store.dir().display());
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    fn role_from_arg(arg: &str) -> String {
        match arg {
            role @ ("auto" | "source" | "target") => role.to_string(),
            other => {
                eprintln!("mouserd: unknown role '{other}', using {}", default_role());
                default_role().to_string()
            }
        }
    }

    fn default_role() -> &'static str {
        #[cfg(target_os = "windows")]
        {
            "target"
        }
        #[cfg(not(target_os = "windows"))]
        {
            "auto"
        }
    }

    /// Connect to an explicit peer (TrustOnFirstUse) and report the handshake, then
    /// exit - a safe transport check that never captures or injects input.
    async fn probe(store: &DaemonStore, addr: SocketAddr) -> Result<(), String> {
        let me = store.load_or_create_identity().map_err(|e| e.to_string())?;
        let endpoint = InteractiveEndpoint::bind_client(SocketAddr::from(([0, 0, 0, 0], 0)))
            .map_err(|e| e.to_string())?;
        eprintln!("mouserd: probing {addr}...");
        let conn = tokio::time::timeout(
            std::time::Duration::from_secs(8),
            endpoint.connect_interactive(&me, addr, PinPolicy::TrustOnFirstUse),
        )
        .await
        .map_err(|_| {
            format!("timed out connecting to {addr} (no Mouser peer there, or UDP blocked)")
        })?
        .map_err(|e| e.to_string())?;
        let alpn = conn
            .negotiated_alpn()
            .map(|b| String::from_utf8_lossy(&b).into_owned());
        let peer = conn.peer_device_id();
        let peer_text = peer
            .as_ref()
            .map(format_device_id)
            .unwrap_or_else(|| "<missing>".to_string());
        eprintln!(
            "mouserd: PROBE OK - handshake with {addr} completed; ALPN={alpn:?}; \
             peer_device_id={peer_text}"
        );
        if let Some(peer_id) = peer {
            eprintln!(
                "mouserd: to trust this peer on this machine, run: mouserd trust {}",
                format_device_id(&peer_id)
            );
        }
        conn.shutdown().await;
        Ok(())
    }

    /// Source mode against an explicit peer address (direct dial, no mDNS): this host
    /// becomes the controller (captures + forwards across the right edge).
    async fn serve_direct(
        store: &DaemonStore,
        addr: SocketAddr,
        expected_peer: DeviceId,
        injector: Arc<dyn InputInjection>,
        capture: Box<dyn InputCapture>,
    ) -> Result<(), String> {
        if !store
            .is_peer_trusted(&expected_peer)
            .map_err(|e| e.to_string())?
        {
            return Err(format!(
                "peer {} is not trusted on this machine; run `mouserd trust {}` first",
                format_device_id(&expected_peer),
                format_device_id(&expected_peer)
            ));
        }

        let me = store.load_or_create_identity().map_err(|e| e.to_string())?;
        let my_id = me.device_id();
        eprintln!("mouserd: device_id {}", me.device_id_base32());
        let endpoint = InteractiveEndpoint::bind_client(SocketAddr::from(([0, 0, 0, 0], 0)))
            .map_err(|e| e.to_string())?;
        eprintln!("mouserd: dialing {addr} directly...");
        let conn = endpoint
            .connect_interactive(&me, addr, PinPolicy::Pinned(expected_peer))
            .await
            .map_err(|e| e.to_string())?;
        let peer = conn
            .peer_device_id()
            .ok_or("peer did not present a device_id")?;
        eprintln!("mouserd: connected directly; this machine can control the peer");

        let core = EngineCore::new_source(my_id, peer, source_layout());
        let runtime = Arc::new(RuntimeHandle::start(core, Arc::new(conn), injector));
        let sink: Arc<dyn InputSink> = Arc::new(EngineSink {
            runtime: Arc::clone(&runtime),
        });
        capture.start(sink).map_err(|e| e.to_string())?;
        eprintln!("mouserd: capture ready - local keys/buttons stay local until edge crossing");

        tokio::signal::ctrl_c().await.map_err(|e| e.to_string())?;
        let _ = capture.stop();
        Ok(())
    }

    /// A serve role (`auto`/`source`/`target`): advertise + discover over mDNS, run the
    /// [`IpcBridge`] so the desktop UI reflects/drives the engine, then form one peer
    /// connection (auto-discovered, accepted, or an IPC `Connect`) and run it until
    /// ctrl-c or an IPC `Disconnect`. Single-session v1, matching the prior behaviour;
    /// the IPC link is the new control surface on top.
    async fn serve(
        store: &DaemonStore,
        role: &str,
        injector: Arc<dyn InputInjection>,
        capture: Box<dyn InputCapture>,
    ) -> Result<(), String> {
        let me = store.load_or_create_identity().map_err(|e| e.to_string())?;
        let my_id = me.device_id();
        let my_b32 = me.device_id_base32();
        eprintln!("mouserd: device_id {my_b32}");
        eprintln!("mouserd: role {role}");

        // One endpoint both accepts (TrustOnFirstUse - trust is the §3 cert pin checked
        // against the mDNS-advertised id) and dials.
        let bind = SocketAddr::from(([0, 0, 0, 0], 0));
        let endpoint = InteractiveEndpoint::bind_server(&me, bind, PinPolicy::TrustOnFirstUse)
            .map_err(|e| e.to_string())?;
        let iport = endpoint.local_addr().map_err(|e| e.to_string())?.port();

        // Advertise this device over mDNS (§4) so the peer can find us.
        let host_ip = discovery::local_ipv4()
            .map(|ip| ip.to_string())
            .unwrap_or_else(|| "0.0.0.0".to_string());
        let advert = discovery::local_advert(&me, &hostname(), iport);
        let _advertiser = Advertiser::advertise(&advert, &host_ip).map_err(|e| e.to_string())?;
        eprintln!(
            "mouserd: advertising {host_ip}:{iport} as {}",
            advert.instance_name()
        );
        windows_firewall_hint(iport);

        // Bring up the local IPC link so the desktop UI can see/drive the engine. The
        // bridge owns its own continuous mDNS browse + IPC server; failure to bind it is
        // non-fatal (the daemon still runs headless), so we warn and carry on.
        let bridge = match IpcBridge::start(store.clone(), my_b32.clone(), hostname()).await {
            Ok(bridge) => Some(bridge),
            Err(e) => {
                eprintln!("mouserd: IPC unavailable ({e}); running headless");
                None
            }
        };

        let browser = Browser::browse().map_err(|e| e.to_string())?;
        eprintln!("mouserd: searching for peers on the local network...");

        // Wait for the first connection to form (auto-dial, accept, or IPC Connect).
        let Some((conn, can_control)) = next_connection(
            store, &endpoint, &me, my_id, &my_b32, &browser, role, bridge.as_ref(),
        )
        .await
        else {
            eprintln!("mouserd: shutting down");
            let _ = capture.stop();
            return Ok(());
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

        run_session(my_id, peer, can_control, conn, injector, capture.as_ref(), bridge.as_ref())
            .await;
        if let Some(bridge) = bridge.as_ref() {
            bridge.set_idle();
        }
        eprintln!("mouserd: shutting down");
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
        browser: &Browser,
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
                    if let Some(peer_id) = ipc {
                        match dial_peer_id(store, endpoint, me, browser, peer_id).await {
                            Ok(conn) => return Some((conn, true)),
                            Err(e) => {
                                eprintln!("mouserd: IPC connect failed: {e}");
                                if let Some(bridge) = bridge { bridge.set_idle(); }
                                continue;
                            }
                        }
                    }
                }
                accepted = accept_trusted(endpoint, store) => {
                    match accepted {
                        Ok(conn) => return Some((conn, false)),
                        Err(e) => { eprintln!("mouserd: accept error: {e}"); continue; }
                    }
                }
                dialed = dial_discovered(store, endpoint, me, my_id, my_b32, browser, role == "source"), if can_dial => {
                    match dialed {
                        Ok(conn) => return Some((conn, true)),
                        Err(e) => { eprintln!("mouserd: dial error: {e}"); continue; }
                    }
                }
            }
        }
    }

    /// Resolve an IPC `Connect` command into the next peer id, or never resolve when
    /// there is no IPC bridge (so the `select!` arm is inert in headless mode).
    async fn wait_for_connect(bridge: Option<&IpcBridge>) -> Option<DeviceId> {
        match bridge {
            Some(bridge) => bridge.next_connect_request().await,
            None => std::future::pending().await,
        }
    }

    /// Dial a specific trusted peer chosen over IPC, resolving its address from the live
    /// discovery registry. Errors if the peer is unknown, not dialable, or untrusted.
    async fn dial_peer_id(
        store: &DaemonStore,
        endpoint: &InteractiveEndpoint,
        me: &DeviceIdentity,
        browser: &Browser,
        peer_id: DeviceId,
    ) -> Result<InteractiveConnection, String> {
        if !store.is_peer_trusted(&peer_id).map_err(|e| e.to_string())? {
            return Err(format!(
                "peer {} is not trusted on this machine",
                format_device_id(&peer_id)
            ));
        }
        // The bridge's registry holds resolved addresses; ask it for this peer's addr.
        let addr = browser_addr_for(browser, &peer_id)
            .await
            .ok_or_else(|| format!("peer {} not currently discoverable", format_device_id(&peer_id)))?;
        eprintln!("mouserd: dialing {addr} (IPC connect)");
        endpoint
            .connect_interactive(me, addr, PinPolicy::Pinned(peer_id))
            .await
            .map_err(|e| e.to_string())
    }

    /// Browse briefly for `peer_id`'s current socket address (used by an IPC dial).
    async fn browser_addr_for(browser: &Browser, peer_id: &DeviceId) -> Option<SocketAddr> {
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            let remaining = deadline.checked_duration_since(tokio::time::Instant::now())?;
            let event = tokio::time::timeout(remaining, browser.next_event()).await.ok()??;
            if let PeerEvent::Found(peer) = event {
                if discovery::peer_device_id(&peer).as_ref() == Some(peer_id) {
                    if let Some(addr) = discovery::peer_socket_addr(&peer) {
                        return Some(addr);
                    }
                }
            }
        }
    }

    /// Run the connected session: start the engine runtime, (for a controller) the
    /// capture hooks, and wait until ctrl-c or an IPC `Disconnect` ends it. The caller
    /// then stops capture and the process exits (single-session v1).
    async fn run_session(
        my_id: DeviceId,
        peer: DeviceId,
        can_control: bool,
        conn: InteractiveConnection,
        injector: Arc<dyn InputInjection>,
        capture: &dyn InputCapture,
        bridge: Option<&IpcBridge>,
    ) {
        let core = if can_control {
            EngineCore::new_source(my_id, peer, source_layout())
        } else {
            EngineCore::new_target(my_id, peer)
        };
        let runtime = Arc::new(RuntimeHandle::start(core, Arc::new(conn), injector));

        if can_control {
            let sink: Arc<dyn InputSink> = Arc::new(EngineSink {
                runtime: Arc::clone(&runtime),
            });
            if let Err(e) = capture.start(sink) {
                eprintln!("mouserd: capture failed to start: {e}");
            } else {
                eprintln!(
                    "mouserd: capture ready - local keys/buttons stay local until edge crossing"
                );
            }
        } else {
            eprintln!("mouserd: target ready - injecting input received from the source");
        }

        // End the session on ctrl-c or an IPC Disconnect.
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = wait_for_disconnect(bridge) => {
                eprintln!("mouserd: disconnect requested over IPC");
            }
        }
        // Keep the runtime alive for the whole session (its tasks own the connection).
        drop(runtime);
    }

    /// Resolve when an IPC `Disconnect` command arrives (inert in headless mode).
    async fn wait_for_disconnect(bridge: Option<&IpcBridge>) {
        match bridge {
            Some(bridge) => bridge.next_disconnect_request().await,
            None => std::future::pending().await,
        }
    }

    fn source_layout() -> EdgeLayout {
        let (width, height) = local_display_size().unwrap_or((1920, 1080));
        EdgeLayout::side_by_side(width, height, 1920, 1080)
    }

    #[cfg(target_os = "windows")]
    fn local_display_size() -> Option<(i32, i32)> {
        platform_win::active_display_bounds()
            .ok()?
            .into_iter()
            .next()
            .map(|display| (display.width, display.height))
    }

    #[cfg(target_os = "macos")]
    fn local_display_size() -> Option<(i32, i32)> {
        platform_mac::active_display_bounds()
            .into_iter()
            .next()
            .map(|display| (display.w.round() as i32, display.h.round() as i32))
    }

    #[cfg(target_os = "linux")]
    fn local_display_size() -> Option<(i32, i32)> {
        None
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
        browser: &Browser,
        force: bool,
    ) -> Result<InteractiveConnection, String> {
        let mut warned_untrusted = BTreeSet::new();
        loop {
            match browser.next_event().await {
                Some(PeerEvent::Found(peer)) if peer.id != my_b32 => {
                    let Some(peer_id) = discovery::peer_device_id(&peer) else {
                        continue;
                    };
                    if !force && my_id >= peer_id {
                        continue; // the peer (lower id) will dial us; we accept instead
                    }
                    if !store.is_peer_trusted(&peer_id).map_err(|e| e.to_string())? {
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
                    let Some(addr) = discovery::peer_socket_addr(&peer) else {
                        continue;
                    };
                    eprintln!("mouserd: dialing {} at {addr}", peer.instance_name());
                    return endpoint
                        .connect_interactive(me, addr, PinPolicy::Pinned(peer_id))
                        .await
                        .map_err(|e| e.to_string());
                }
                Some(_) => continue,
                None => return Err("mDNS browse channel closed".to_string()),
            }
        }
    }

    /// Accept inbound connections until the peer id is explicitly trusted locally.
    /// Untrusted peers can complete the transport handshake (so `probe` can discover
    /// their id), but they are closed before the engine runtime can inject anything.
    async fn accept_trusted(
        endpoint: &InteractiveEndpoint,
        store: &DaemonStore,
    ) -> Result<InteractiveConnection, String> {
        loop {
            let conn = endpoint
                .accept_interactive()
                .await
                .map_err(|e| e.to_string())?;
            let Some(peer_id) = conn.peer_device_id() else {
                eprintln!("mouserd: rejected peer without a valid device_id");
                conn.close();
                continue;
            };
            if store.is_peer_trusted(&peer_id).map_err(|e| e.to_string())? {
                return Ok(conn);
            }

            let peer_text = format_device_id(&peer_id);
            eprintln!(
                "mouserd: rejected untrusted peer {peer_text}; run `mouserd trust {peer_text}` \
                 on this machine to allow control"
            );
            conn.close();
        }
    }

    /// Best-effort host display name for the advertisement (advisory only, §4).
    fn hostname() -> String {
        std::env::var("COMPUTERNAME")
            .or_else(|_| std::env::var("HOST"))
            .or_else(|_| std::env::var("HOSTNAME"))
            .ok()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "mouser".to_string())
    }

    #[cfg(target_os = "windows")]
    fn windows_firewall_hint(_iport: u16) {
        let exe = std::env::current_exe()
            .ok()
            .and_then(|p| p.into_os_string().into_string().ok())
            .unwrap_or_else(|| "mouserd.exe".to_string());
        eprintln!(
            "mouserd: Windows Firewall must allow inbound UDP for mDNS/QUIC. \
             If Windows prompts, allow Private networks. If peers do not appear, \
             run elevated PowerShell:\n  netsh advfirewall firewall add rule \
             name=\"Mouser daemon UDP\" dir=in action=allow program=\"{exe}\" \
             protocol=UDP profile=private"
        );
    }

    #[cfg(not(target_os = "windows"))]
    fn windows_firewall_hint(_iport: u16) {}
}

/// The IPC bridge: a continuous mDNS browse → peer registry, a [`mouser_ipc::Server`]
/// publishing snapshots on change, and the channels the serve loop uses to learn about
/// UI `Connect`/`Disconnect` commands and to report connection state.
#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
mod ipc_bridge {
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

    use mouser_core::DeviceId;
    use mouser_engine::daemon_store::DaemonStore;
    use mouser_engine::discovery;
    use mouser_ipc::{
        Command, ConnectionDto, ConnectionStateDto, DeviceDto, PeerDto, Publisher, Server, Snapshot,
    };
    use mouser_net::{Browser, PeerAdvert, PeerEvent};
    use tokio::sync::mpsc;

    /// OS kind advertised for the local device DTO (matches the frontend `OsKind`).
    const OS_KIND: &str = if cfg!(target_os = "macos") {
        "macos"
    } else if cfg!(target_os = "windows") {
        "windows"
    } else {
        "linux"
    };

    /// Shared, mutable engine state the snapshot is built from.
    struct Shared {
        store: DaemonStore,
        local: DeviceDto,
        /// Discovered peers keyed by DNS-SD instance fullname.
        peers: Mutex<HashMap<String, PeerAdvert>>,
        connection: Mutex<ConnectionDto>,
    }

    fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
        m.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The running IPC bridge handle the serve loop drives.
    pub struct IpcBridge {
        shared: Arc<Shared>,
        /// Cheap publish handle (no lock contention with the command-receiving task).
        publisher: Publisher,
        // `tokio::sync::Mutex` so the single consumer (the serve loop) can hold the
        // guard across the `recv().await` without breaking `Send`.
        connect_rx: tokio::sync::Mutex<mpsc::UnboundedReceiver<DeviceId>>,
        disconnect_rx: tokio::sync::Mutex<mpsc::UnboundedReceiver<()>>,
        tasks: Vec<tokio::task::JoinHandle<()>>,
    }

    impl IpcBridge {
        /// Start the bridge: bind the IPC server, spawn the browse + command loops.
        pub async fn start(
            store: DaemonStore,
            local_id: String,
            local_name: String,
        ) -> Result<Self, String> {
            let shared = Arc::new(Shared {
                store,
                local: DeviceDto {
                    id: local_id,
                    name: local_name,
                    os: OS_KIND.to_string(),
                },
                peers: Mutex::new(HashMap::new()),
                connection: Mutex::new(ConnectionDto::default()),
            });

            let server = Server::bind(build_snapshot(&shared))
                .await
                .map_err(|e| e.to_string())?;
            eprintln!("mouserd: IPC listening at {}", server.socket_path().display());
            let publisher = server.publisher();

            let (connect_tx, connect_rx) = mpsc::unbounded_channel();
            let (disconnect_tx, disconnect_rx) = mpsc::unbounded_channel();

            // The command loop owns the `Server` (it awaits `recv_command`); the bridge
            // and browse loop publish through cloned `Publisher`s, so reporting state
            // never contends with command reception.
            let tasks = vec![
                tokio::spawn(browse_loop(Arc::clone(&shared), publisher.clone())),
                tokio::spawn(command_loop(server, connect_tx, disconnect_tx)),
            ];

            Ok(Self {
                shared,
                publisher,
                connect_rx: tokio::sync::Mutex::new(connect_rx),
                disconnect_rx: tokio::sync::Mutex::new(disconnect_rx),
                tasks,
            })
        }

        /// Await the next UI `Connect{peer_id}` request (decoded to a `DeviceId`).
        pub async fn next_connect_request(&self) -> Option<DeviceId> {
            // The receiver is single-consumer; the serve loop is the only caller.
            let mut guard = self.connect_rx.lock().await;
            guard.recv().await
        }

        /// Await the next UI `Disconnect` request.
        pub async fn next_disconnect_request(&self) {
            let mut guard = self.disconnect_rx.lock().await;
            let _ = guard.recv().await;
        }

        /// Report that the engine connected to `peer_id`; republish the snapshot.
        pub fn set_connected(&self, peer_id: &str, owner_id: &str) {
            *lock(&self.shared.connection) = ConnectionDto {
                state: ConnectionStateDto::Connected,
                peer_id: Some(peer_id.to_string()),
                owner: Some(owner_id.to_string()),
                epoch: None,
            };
            self.republish();
        }

        /// Report that the engine has no connection; republish the snapshot.
        pub fn set_idle(&self) {
            *lock(&self.shared.connection) = ConnectionDto::default();
            self.republish();
        }

        fn republish(&self) {
            self.publisher.publish(build_snapshot(&self.shared));
        }
    }

    impl Drop for IpcBridge {
        fn drop(&mut self) {
            for task in self.tasks.drain(..) {
                task.abort();
            }
        }
    }

    /// Build a fresh snapshot from the shared state (local + discovered peers + trust).
    fn build_snapshot(shared: &Shared) -> Snapshot {
        let peers_guard = lock(&shared.peers);
        let mut peers: Vec<PeerDto> = peers_guard
            .values()
            .map(|advert| {
                let trusted = discovery::peer_device_id(advert)
                    .map(|id| shared.store.is_peer_trusted(&id).unwrap_or(false))
                    .unwrap_or(false);
                let host = advert
                    .addrs
                    .first()
                    .map(|ip| ip.to_string())
                    .unwrap_or_default();
                PeerDto {
                    id: advert.id.clone(),
                    name: if advert.name.is_empty() {
                        host.clone()
                    } else {
                        advert.name.clone()
                    },
                    os: advert.os.clone(),
                    host,
                    port: advert.iport,
                    trusted,
                }
            })
            .collect();
        peers.sort_by(|a, b| a.id.cmp(&b.id));
        Snapshot {
            local: shared.local.clone(),
            peers,
            connection: lock(&shared.connection).clone(),
        }
    }

    /// Continuous mDNS browse: fold `Found`/`Removed` into the peer registry and
    /// republish a snapshot on every change so connected UIs stay live.
    async fn browse_loop(shared: Arc<Shared>, publisher: Publisher) {
        let browser = match Browser::browse() {
            Ok(b) => b,
            Err(_) => return, // no mDNS daemon: leave peers empty
        };
        while let Some(event) = browser.next_event().await {
            let changed = match event {
                PeerEvent::Found(advert) => {
                    if advert.id == shared.local.id {
                        false // never list ourselves
                    } else {
                        let fullname =
                            format!("{}.{}", advert.instance_name(), mouser_net::SERVICE_TYPE);
                        lock(&shared.peers).insert(fullname, advert);
                        true
                    }
                }
                PeerEvent::Removed(fullname) => lock(&shared.peers).remove(&fullname).is_some(),
            };
            if changed {
                publisher.publish(build_snapshot(&shared));
            }
        }
    }

    /// Drain UI commands from the IPC server and forward Connect/Disconnect to the serve
    /// loop. `GetSnapshot` is handled inside the server itself.
    async fn command_loop(
        mut server: Server,
        connect_tx: mpsc::UnboundedSender<DeviceId>,
        disconnect_tx: mpsc::UnboundedSender<()>,
    ) {
        loop {
            match server.recv_command().await {
                Some(Command::Connect { peer_id }) => match discovery::decode_device_id(&peer_id) {
                    Some(id) => {
                        let _ = connect_tx.send(id);
                    }
                    None => eprintln!("mouserd: IPC Connect with invalid peer id: {peer_id}"),
                },
                Some(Command::Disconnect) => {
                    let _ = disconnect_tx.send(());
                }
                // GetSnapshot is answered by the server; nothing reaches here.
                Some(Command::GetSnapshot) => {}
                None => return, // server dropped
            }
        }
    }
}
