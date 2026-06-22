//! `mouserd` — the Mouser engine daemon.
//!
//! v1 single-peer bring-up: **auto-discover a peer over mDNS** (`_mouser._udp.local`,
//! §4), establish the device_id-pinned interactive QUIC connection (§3), and run the
//! [`mouser_engine`] runtime wired to the host's real capture/injection adapters.
//! On macOS that is `platform-mac` (`MacCapture` + `MacInjector`); on Windows it is
//! `platform-win` (`WinCapture` + `WinInjector`).
//!
//! Usage:
//! - `mouserd`          — auto: advertise + browse; the lower device_id becomes source.
//! - `mouserd source`   — be the controller (capture + dial the discovered peer).
//! - `mouserd target`   — be the controlled screen (accept + inject).
//!
//! Discovery itself is platform-agnostic (it lives in the shared `mouser-engine`/
//! `mouser-net` crates over `mdns-sd`). The §5 SAS pairing UI, CRDT layout, and
//! Tauri/IPC sit above.

fn main() {
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    {
        desktop::run();
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        eprintln!(
            "mouserd: this host's platform adapters are not wired into the daemon yet. \
             The engine library is platform-agnostic."
        );
        std::process::exit(1);
    }
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
mod desktop {
    use std::net::SocketAddr;
    use std::sync::Arc;

    use mouser_core::platform::{
        CaptureDecision as CoreDecision, InputCapture, InputSink, LocalInputEvent,
    };
    use mouser_core::DeviceId;
    use mouser_engine::core::CaptureDecision;
    use mouser_engine::{discovery, EdgeLayout, EngineCore, RuntimeHandle};
    use mouser_net::{
        Advertiser, Browser, DeviceIdentity, InteractiveConnection, InteractiveEndpoint, PeerEvent,
        PinPolicy,
    };

    #[cfg(target_os = "macos")]
    use platform_mac::adapter::{MacCapture as PlatformCapture, MacInjector as PlatformInjector};
    #[cfg(target_os = "windows")]
    use platform_win::{WinCapture as PlatformCapture, WinInjector as PlatformInjector};

    #[cfg(target_os = "macos")]
    const DEFAULT_HOSTNAME: &str = "Mac";
    #[cfg(target_os = "windows")]
    const DEFAULT_HOSTNAME: &str = "Windows";

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

    pub fn run() {
        let args: Vec<String> = std::env::args().collect();
        // Usage: mouserd [source|target]  (default: auto)
        let role = args.get(1).cloned().unwrap_or_else(|| "auto".to_string());

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

        rt.block_on(async move {
            if let Err(e) = serve(&role).await {
                eprintln!("mouserd: {e}");
                std::process::exit(1);
            }
        });
    }

    async fn serve(role: &str) -> Result<(), String> {
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
        eprintln!("mouserd: searching for peers on the local network...");

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
                accepted = endpoint.accept_interactive() => (
                    accepted.map_err(|e| e.to_string())?,
                    false
                ),
                dialed = dial_discovered(&endpoint, &me, my_id, &my_b32, &browser, false) => (
                    dialed?,
                    true
                ),
            },
        };

        let peer = conn
            .peer_device_id()
            .ok_or("peer did not present a device_id")?;
        eprintln!(
            "mouserd: connected as {}",
            if is_source {
                "source (controller)"
            } else {
                "target (controlled)"
            }
        );

        let injector = Arc::new(PlatformInjector::new());
        let core = if is_source {
            EngineCore::new_source(my_id, peer, EdgeLayout::side_by_side(1512, 982, 1920, 1080))
        } else {
            EngineCore::new_target(my_id, peer)
        };
        let runtime = Arc::new(RuntimeHandle::start(core, Arc::new(conn), injector));

        if is_source {
            let capture = PlatformCapture::new();
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

    /// Best-effort host display name for the advertisement (advisory only, §4).
    fn hostname() -> String {
        std::env::var("COMPUTERNAME")
            .or_else(|_| std::env::var("HOST"))
            .or_else(|_| std::env::var("HOSTNAME"))
            .ok()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| DEFAULT_HOSTNAME.to_string())
    }
}
