//! The Mouser engine daemon, platform-agnostic.
//!
//! [`run`] is the entry point the `mouserd` binary calls with the host's concrete
//! capture/injection adapters (the only per-OS difference). It dispatches the CLI:
//! local commands (`identity`/`trust`/`trusted`), the direct explicit-address modes
//! (`probe`/`connect`, see [`direct`]), and the mDNS serve roles (`auto`/`source`/
//! `target`, see [`serve`]). While serving it also runs the [`ipc_bridge`] so the
//! Tauri desktop UI can reflect and drive the engine.

mod clipboard;
mod clipboard_bulk;
mod direct;
mod file_transfer;
mod ipc_bridge;
mod ipc_health;
mod pairing;
mod reconnect;
mod serve;
mod serve_session;

use std::net::SocketAddr;
use std::sync::Arc;

use mouser_core::platform::{Clipboard, InputCapture, InputInjection};

use crate::daemon_store::{format_device_id, parse_peer_id_arg, DaemonStore};
use crate::EdgeLayout;

pub use file_transfer::send_paths_to_peer;

/// Run the daemon with the host's `injector` and `capture` adapters.
///
/// `capture` is an [`Arc`] because the engine runtime holds it for the session
/// lifetime to drive [`mouser_core::platform::CaptureMode`] transitions (the
/// runtime, not the daemon, decides when forwarding hooks install) while the serve
/// loop keeps its own handle to stop it on shutdown.
pub fn run(
    injector: Arc<dyn InputInjection>,
    capture: Arc<dyn InputCapture>,
    clipboard: Arc<dyn Clipboard>,
) {
    let args: Vec<String> = std::env::args().collect();
    let arg1 = args
        .get(1)
        .cloned()
        .unwrap_or_else(|| default_role().to_string());
    let store = match DaemonStore::open_default() {
        Ok(store) => store,
        Err(e) => {
            crate::diag!(info, "mouserd: {e}");
            std::process::exit(1);
        }
    };

    match handle_local_command(&arg1, &args, &store) {
        Ok(true) => return,
        Ok(false) => {}
        Err(e) => {
            crate::diag!(info, "mouserd: {e}");
            std::process::exit(1);
        }
    }

    let rt = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            crate::diag!(info, "mouserd: failed to start tokio runtime: {e}");
            std::process::exit(1);
        }
    };

    // Direct modes take an explicit peer address and bypass mDNS (and IPC).
    if arg1 == "probe" || arg1 == "connect" {
        let Some(addr_str) = args.get(2).cloned() else {
            crate::diag!(
                info,
                "mouserd: `{arg1}` needs <host:port>, e.g. mouserd {arg1} 192.168.1.230:49970"
            );
            std::process::exit(1);
        };
        let addr: SocketAddr = match addr_str.parse() {
            Ok(a) => a,
            Err(e) => {
                crate::diag!(info, "mouserd: bad address {addr_str}: {e}");
                std::process::exit(1);
            }
        };
        rt.block_on(async move {
            let result = if arg1 == "probe" {
                direct::probe(&store, addr).await
            } else if let Some(peer_id_arg) = args.get(3).cloned() {
                match parse_peer_id_arg(&peer_id_arg) {
                    Ok(peer_id) => {
                        direct::serve_direct(&store, addr, peer_id, injector, capture, clipboard)
                            .await
                    }
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
                crate::diag!(info, "mouserd: {e}");
                std::process::exit(1);
            }
        });
        return;
    }

    let role = role_from_arg(&arg1);
    rt.block_on(async move {
        if let Err(e) = serve::serve(&store, &role, injector, capture, clipboard).await {
            crate::diag!(info, "mouserd: {e}");
            std::process::exit(1);
        }
    });
}

/// Run the engine IN-PROCESS, for a host (the desktop app) that runs the engine itself
/// instead of spawning a separate `mouserd` process. Same mDNS serve loop + IPC bridge as
/// [`run`], but the CALLER owns the tokio runtime and lifecycle: spawn this as a task on
/// the app's runtime, and on quit abort the task and call `capture.stop()` to release any
/// input hooks (the serve loop's own `ctrl_c` arms never fire in a windowed app). Returns
/// when the serve loop ends. `role` is `auto`/`source`/`target` (invalid falls back to
/// `auto`). No CLI parsing, no `std::process::exit` — those stay in [`run`].
pub async fn run_engine(
    store: DaemonStore,
    role: String,
    injector: Arc<dyn InputInjection>,
    capture: Arc<dyn InputCapture>,
    clipboard: Arc<dyn Clipboard>,
) -> Result<(), String> {
    let role = role_from_arg(&role);
    serve::serve(&store, &role, injector, capture, clipboard).await
}

/// Handle the side-effect-free local commands (`identity`/`trust`/`trusted`) that never
/// open a socket. Returns `Ok(true)` if a command was handled (the daemon should exit).
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
            crate::diag!(
                info,
                "mouserd: unknown role '{other}', using {}",
                default_role()
            );
            default_role().to_string()
        }
    }
}

fn default_role() -> &'static str {
    // `auto` on every platform: a node both advertises and browses, and either side
    // can become the controller. This used to default to `target` on Windows as a
    // safety workaround because becoming a source installed always-on low-level
    // hooks that degraded local input. Capture is now ownership-driven (passive edge
    // sensing while connected, suppressing hooks only while actively controlling), so
    // Windows is a first-class source and no longer needs the workaround.
    "auto"
}

/// The source-side edge layout, seeded from the local display size when available.
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
    platform_linux::active_display_bounds()
        .ok()?
        .into_iter()
        .next()
        .map(|display| (display.width, display.height))
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
    crate::diag!(
        info,
        "mouserd: Windows Firewall must allow inbound UDP for mDNS/QUIC. \
         If Windows prompts, allow Private networks. If peers do not appear, \
         run elevated PowerShell:\n  netsh advfirewall firewall add rule \
         name=\"Mouser daemon UDP\" dir=in action=allow program=\"{exe}\" \
         protocol=UDP profile=private"
    );
}

#[cfg(not(target_os = "windows"))]
fn windows_firewall_hint(_iport: u16) {}
