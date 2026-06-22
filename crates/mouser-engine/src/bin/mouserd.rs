//! `mouserd` — the Mouser engine daemon.
//!
//! v1 single-peer bring-up: **auto-discover a peer over mDNS** (`_mouser._udp.local`,
//! §4), establish the device_id-pinned interactive QUIC connection (§3), and run the
//! [`mouser_engine`] runtime wired to the host's real capture/injection adapters:
//! - macOS → `platform-mac` (`MacCapture` + `MacInjector`),
//! - Windows → `platform-win` (`WinCapture` + `WinInjector`),
//! - Linux → `platform-linux` (`LinuxCapture` + `UinputInjector`).
//!
//! Usage:
//! - `mouserd`          — auto: advertise + browse; the lower device_id becomes source.
//! - `mouserd source`   — be the controller (capture + dial the discovered peer).
//! - `mouserd target`   — be the controlled screen (accept + inject).
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
    use std::net::SocketAddr;
    use std::sync::Arc;

    use mouser_core::platform::{
        CaptureDecision as CoreDecision, InputCapture, InputInjection, InputSink, LocalInputEvent,
    };
    use mouser_core::DeviceId;
    use mouser_engine::core::CaptureDecision;
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
    ///
    /// Usage:
    /// - `mouserd [source|target|auto]` — mDNS auto-discovery (default `auto`).
    /// - `mouserd connect <host:port>`  — dial an explicit peer (no mDNS) and be the
    ///   source/controller; useful when mDNS doesn't traverse the network.
    /// - `mouserd probe <host:port>`    — connect, report the handshake, and exit
    ///   WITHOUT capturing/injecting (a safe cross-machine transport check).
    pub fn run(injector: Arc<dyn InputInjection>, capture: Box<dyn InputCapture>) {
        let args: Vec<String> = std::env::args().collect();
        let arg1 = args.get(1).cloned().unwrap_or_else(|| "auto".to_string());

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
                eprintln!("mouserd: `{arg1}` needs <host:port>, e.g. mouserd {arg1} 192.168.1.230:49970");
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
                    probe(addr).await
                } else {
                    serve_direct(addr, injector, capture).await
                };
                if let Err(e) = result {
                    eprintln!("mouserd: {e}");
                    std::process::exit(1);
                }
            });
            return;
        }

        rt.block_on(async move {
            if let Err(e) = serve(&arg1, injector, capture).await {
                eprintln!("mouserd: {e}");
                std::process::exit(1);
            }
        });
    }

    /// Connect to an explicit peer (TrustOnFirstUse) and report the handshake, then
    /// exit — a safe transport check that never captures or injects input.
    async fn probe(addr: SocketAddr) -> Result<(), String> {
        let me = DeviceIdentity::generate();
        let endpoint = InteractiveEndpoint::bind_client(SocketAddr::from(([0, 0, 0, 0], 0)))
            .map_err(|e| e.to_string())?;
        eprintln!("mouserd: probing {addr} …");
        let conn = tokio::time::timeout(
            std::time::Duration::from_secs(8),
            endpoint.connect_interactive(&me, addr, PinPolicy::TrustOnFirstUse),
        )
        .await
        .map_err(|_| format!("timed out connecting to {addr} (no Mouser peer there, or UDP blocked)"))?
        .map_err(|e| e.to_string())?;
        let alpn = conn
            .negotiated_alpn()
            .map(|b| String::from_utf8_lossy(&b).into_owned());
        eprintln!(
            "mouserd: PROBE OK — handshake with {addr} completed; ALPN={alpn:?}; peer_device_id_present={}",
            conn.peer_device_id().is_some()
        );
        conn.shutdown().await;
        Ok(())
    }

    /// Source mode against an explicit peer address (direct dial, no mDNS): this host
    /// becomes the controller (captures + forwards across the right edge).
    async fn serve_direct(
        addr: SocketAddr,
        injector: Arc<dyn InputInjection>,
        capture: Box<dyn InputCapture>,
    ) -> Result<(), String> {
        let me = DeviceIdentity::generate();
        let my_id = me.device_id();
        eprintln!("mouserd: device_id {}", me.device_id_base32());
        let endpoint = InteractiveEndpoint::bind_client(SocketAddr::from(([0, 0, 0, 0], 0)))
            .map_err(|e| e.to_string())?;
        eprintln!("mouserd: dialing {addr} directly…");
        let conn = endpoint
            .connect_interactive(&me, addr, PinPolicy::TrustOnFirstUse)
            .await
            .map_err(|e| e.to_string())?;
        let peer = conn.peer_device_id().ok_or("peer did not present a device_id")?;
        eprintln!("mouserd: connected as source (controller)");
        let core =
            EngineCore::new_source(my_id, peer, EdgeLayout::side_by_side(1512, 982, 1920, 1080));
        let runtime = Arc::new(RuntimeHandle::start(core, Arc::new(conn), injector));
        let sink: Arc<dyn InputSink> = Arc::new(EngineSink {
            runtime: Arc::clone(&runtime),
        });
        capture.start(sink).map_err(|e| e.to_string())?;
        eprintln!("mouserd: capturing — move the cursor to the right edge to cross to the peer");
        tokio::signal::ctrl_c().await.map_err(|e| e.to_string())?;
        let _ = capture.stop();
        Ok(())
    }

    async fn serve(
        role: &str,
        injector: Arc<dyn InputInjection>,
        capture: Box<dyn InputCapture>,
    ) -> Result<(), String> {
        let me = DeviceIdentity::generate();
        let my_id = me.device_id();
        let my_b32 = me.device_id_base32();
        eprintln!("mouserd: device_id {my_b32}");

        // One endpoint both accepts (TrustOnFirstUse — trust is the §3 cert pin checked
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

        let browser = Browser::browse().map_err(|e| e.to_string())?;
        eprintln!("mouserd: searching for peers on the local network…");

        // Form exactly one connection per role (whichever side dials/accepts).
        let (conn, is_source) = match role {
            "source" => (
                dial_discovered(&endpoint, &me, my_id, &my_b32, &browser, true).await?,
                true,
            ),
            "target" => (
                endpoint
                    .accept_interactive()
                    .await
                    .map_err(|e| e.to_string())?,
                false,
            ),
            _ => tokio::select! {
                accepted = endpoint.accept_interactive() => (accepted.map_err(|e| e.to_string())?, false),
                dialed = dial_discovered(&endpoint, &me, my_id, &my_b32, &browser, false) => (dialed?, true),
            },
        };

        let peer = conn.peer_device_id().ok_or("peer did not present a device_id")?;
        eprintln!(
            "mouserd: connected as {}",
            if is_source {
                "source (controller)"
            } else {
                "target (controlled)"
            }
        );

        let core = if is_source {
            EngineCore::new_source(my_id, peer, EdgeLayout::side_by_side(1512, 982, 1920, 1080))
        } else {
            EngineCore::new_target(my_id, peer)
        };
        let runtime = Arc::new(RuntimeHandle::start(core, Arc::new(conn), injector));

        if is_source {
            let sink: Arc<dyn InputSink> = Arc::new(EngineSink {
                runtime: Arc::clone(&runtime),
            });
            capture.start(sink).map_err(|e| e.to_string())?;
            eprintln!(
                "mouserd: capturing — move the cursor to the right edge to cross to the peer"
            );
        } else {
            eprintln!("mouserd: target ready — injecting input received from the source");
        }

        tokio::signal::ctrl_c().await.map_err(|e| e.to_string())?;
        eprintln!("mouserd: shutting down");
        // Restore local input (ungrab on Linux / drop the tap on macOS).
        let _ = capture.stop();
        Ok(())
    }

    /// Browse mDNS until a dialable peer appears and dial it (device_id-pinned, §3).
    /// When `force` is false (auto mode) only the lower `device_id` dials, so the two
    /// sides don't connect twice.
    async fn dial_discovered(
        endpoint: &InteractiveEndpoint,
        me: &DeviceIdentity,
        my_id: DeviceId,
        my_b32: &str,
        browser: &Browser,
        force: bool,
    ) -> Result<InteractiveConnection, String> {
        loop {
            match browser.next_event().await {
                Some(PeerEvent::Found(peer)) if peer.id != my_b32 => {
                    let Some(peer_id) = discovery::peer_device_id(&peer) else {
                        continue;
                    };
                    if !force && my_id >= peer_id {
                        continue; // the peer (lower id) will dial us; we accept instead
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

    /// Best-effort host display name for the advertisement (advisory only, §4).
    fn hostname() -> String {
        std::env::var("HOST")
            .or_else(|_| std::env::var("HOSTNAME"))
            .ok()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "mouser".to_string())
    }
}
