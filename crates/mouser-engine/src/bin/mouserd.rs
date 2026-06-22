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
//! Discovery itself is platform-agnostic (it lives in the shared `mouser-engine`/
//! `mouser-net` crates over `mdns-sd`), so the same serve loop runs on every host;
//! only the concrete capture/injection adapters differ per OS (selected in `main`).
//! The §5 SAS pairing UI, CRDT layout, and Tauri/IPC sit above.

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

        // Direct modes take an explicit peer address and bypass mDNS.
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

        // Form exactly one connection per role (whichever side dials/accepts).
        // Connection direction only avoids duplicate QUIC sessions; control ability
        // comes from the selected role below.
        let (conn, direction) = match role {
            "source" => (
                dial_discovered(
                    store,
                    &endpoint,
                    &me,
                    my_id,
                    &my_b32,
                    &Browser::browse().map_err(|e| e.to_string())?,
                    true,
                )
                .await?,
                "dialed",
            ),
            "target" => (accept_trusted(&endpoint, store).await?, "accepted"),
            _ => {
                let browser = Browser::browse().map_err(|e| e.to_string())?;
                eprintln!("mouserd: searching for peers on the local network...");
                tokio::select! {
                    accepted = accept_trusted(&endpoint, store) => (
                        accepted?,
                        "accepted"
                    ),
                    dialed = dial_discovered(store, &endpoint, &me, my_id, &my_b32, &browser, false) => (
                        dialed?,
                        "dialed"
                    ),
                }
            }
        };

        let peer = conn
            .peer_device_id()
            .ok_or("peer did not present a device_id")?;
        let can_control = role != "target";
        eprintln!(
            "mouserd: connected ({direction}); {}",
            if can_control {
                "this machine can control the peer"
            } else {
                "receive-only target mode"
            }
        );

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
            capture.start(sink).map_err(|e| e.to_string())?;
            eprintln!("mouserd: capture ready - local keys/buttons stay local until edge crossing");
        } else {
            eprintln!("mouserd: target ready - injecting input received from the source");
        }

        tokio::signal::ctrl_c().await.map_err(|e| e.to_string())?;
        eprintln!("mouserd: shutting down");
        // Restore local input (ungrab on Linux / drop the tap on macOS).
        let _ = capture.stop();
        Ok(())
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
