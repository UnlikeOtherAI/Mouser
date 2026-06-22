//! `mouserd` — the Mouser engine daemon.
//!
//! v1 single-peer bring-up: discover or dial a peer, establish the interactive QUIC
//! connection (device_id-pinned, §3), and run the [`mouser_engine`] runtime wired to
//! the host's real capture/injection adapters. On macOS that is `platform-mac`
//! (`MacCapture` + `MacInjector`); other hosts are not wired here yet (the Windows
//! adapter is built but plugged in from a Windows build).
//!
//! This is intentionally minimal — argument parsing, the §5 SAS pairing UI, mDNS
//! auto-discovery, and the Tauri/IPC surface land on top of the runtime, not inside it.

fn main() {
    #[cfg(target_os = "macos")]
    {
        macos::run();
    }
    #[cfg(not(target_os = "macos"))]
    {
        eprintln!(
            "mouserd: this host's platform adapters are not wired into the daemon yet \
             (macOS is the v1 bring-up target). The engine library is platform-agnostic."
        );
        std::process::exit(1);
    }
}

#[cfg(target_os = "macos")]
mod macos {
    use std::sync::Arc;

    use mouser_core::platform::{CaptureDecision as CoreDecision, InputCapture, InputSink, LocalInputEvent};
    use mouser_engine::core::CaptureDecision;
    use mouser_engine::{EdgeLayout, EngineCore, RuntimeHandle};
    use mouser_net::{DeviceIdentity, InteractiveEndpoint, PinPolicy};
    use platform_mac::adapter::{MacCapture, MacInjector};

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
        // Usage: mouserd <listen|connect> [peer_addr]
        let mode = args.get(1).cloned().unwrap_or_else(|| "listen".to_string());
        let peer_addr = args.get(2).cloned();

        let rt = match tokio::runtime::Builder::new_multi_thread().enable_all().build() {
            Ok(rt) => rt,
            Err(e) => {
                eprintln!("mouserd: failed to start tokio runtime: {e}");
                std::process::exit(1);
            }
        };

        rt.block_on(async move {
            if let Err(e) = serve(&mode, peer_addr).await {
                eprintln!("mouserd: {e}");
                std::process::exit(1);
            }
        });
    }

    async fn serve(mode: &str, peer_addr: Option<String>) -> Result<(), String> {
        let me = DeviceIdentity::generate();
        eprintln!("mouserd: device_id {}", me.device_id_base32());

        // v1: pin-on-first-contact is approximated by accepting the peer's presented
        // cert and deriving its device_id; real trust requires the §5 SAS pairing.
        let conn = match mode {
            "connect" => {
                let addr = peer_addr
                    .ok_or("connect mode needs a peer address, e.g. mouserd connect 192.168.1.50:9000")?
                    .parse()
                    .map_err(|e| format!("bad peer address: {e}"))?;
                let endpoint = InteractiveEndpoint::bind_client(mouser_net::loopback_addr())
                    .map_err(|e| e.to_string())?;
                // Pin the peer once we learn it; for bring-up we expect a known id via env.
                let peer = expected_peer()?;
                endpoint
                    .connect_interactive(&me, addr, PinPolicy::Pinned(peer))
                    .await
                    .map_err(|e| e.to_string())?
            }
            _ => {
                let peer = expected_peer()?;
                let addr = "0.0.0.0:9000".parse().map_err(|e| format!("addr: {e}"))?;
                let endpoint = InteractiveEndpoint::bind_server(&me, addr, PinPolicy::Pinned(peer))
                    .map_err(|e| e.to_string())?;
                eprintln!("mouserd: listening on {}", endpoint.local_addr().map_err(|e| e.to_string())?);
                endpoint.accept_interactive().await.map_err(|e| e.to_string())?
            }
        };

        let peer = conn.peer_device_id().ok_or("peer did not present a pinned device_id")?;
        eprintln!("mouserd: connected to peer; starting engine");

        let injector = Arc::new(MacInjector::new());
        let core = if mode == "connect" {
            // The dialer is the source (has the keyboard/mouse) in this bring-up.
            EngineCore::new_source(me.device_id(), peer, EdgeLayout::side_by_side(1512, 982, 1920, 1080))
        } else {
            EngineCore::new_target(me.device_id(), peer)
        };

        let runtime = Arc::new(RuntimeHandle::start(core, Arc::new(conn), injector));

        // The source captures local input and feeds it to the engine.
        if mode == "connect" {
            let capture = MacCapture::new();
            let sink: Arc<dyn InputSink> = Arc::new(EngineSink { runtime: Arc::clone(&runtime) });
            capture.start(sink).map_err(|e| e.to_string())?;
            eprintln!("mouserd: capturing local input (move cursor to the right edge to cross)");
        } else {
            eprintln!("mouserd: target ready; will inject input received from the source");
        }

        // Run until interrupted.
        tokio::signal::ctrl_c().await.map_err(|e| e.to_string())?;
        eprintln!("mouserd: shutting down");
        Ok(())
    }

    /// v1 bring-up pins the peer's device_id supplied out-of-band via `MOUSER_PEER`
    /// (base32). Real discovery + SAS pairing replace this.
    fn expected_peer() -> Result<[u8; 32], String> {
        let raw = std::env::var("MOUSER_PEER")
            .map_err(|_| "set MOUSER_PEER to the peer's device_id (base32) for v1 bring-up")?;
        decode_base32_id(&raw)
    }

    fn decode_base32_id(s: &str) -> Result<[u8; 32], String> {
        // RFC 4648 base32 (no padding, lowercase), matching `device_id_base32`.
        const ALPHABET: &[u8; 32] = b"abcdefghijklmnopqrstuvwxyz234567";
        let mut bits = 0u32;
        let mut nbits = 0u32;
        let mut out = Vec::with_capacity(32);
        for ch in s.trim().bytes() {
            let val = ALPHABET.iter().position(|&c| c == ch).ok_or("invalid base32 char")? as u32;
            bits = (bits << 5) | val;
            nbits += 5;
            if nbits >= 8 {
                nbits -= 8;
                out.push(((bits >> nbits) & 0xFF) as u8);
            }
        }
        <[u8; 32]>::try_from(out.as_slice()).map_err(|_| "device_id must be 32 bytes".to_string())
    }
}
