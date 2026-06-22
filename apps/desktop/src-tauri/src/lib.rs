//! Mouser desktop UI shell (Tauri v2).
//!
//! Per `docs/tech-stack.md` §5 and `docs/architecture.md` §3/§8 this crate does
//! NOT own the daemon lifecycle and does NOT embed `mouser-core`. When wiring
//! lands it will talk to the engine over `mouser-ipc` (typed DTOs).
//!
//! Until then it still surfaces the **real local machine** — its name, OS and
//! physical display arrangement — via [`local_device`] so the Devices list and
//! Layout canvas reflect this computer instead of placeholder data. Remote peers
//! still require the engine; the UI shows an honest "no peers yet" state.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use mouser_net::{Browser, PeerEvent};
use serde::Serialize;
use tauri::{
    menu::MenuBuilder,
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    Manager, WindowEvent,
};

/// Compile-time OS kind, matching the frontend `OsKind` union.
const OS_KIND: &str = if cfg!(target_os = "macos") {
    "macos"
} else if cfg!(target_os = "windows") {
    "windows"
} else {
    "linux"
};

/// One physical display, in DPI-normalized **logical points** so screens of
/// different scale factors lay out 1:1 with how the OS arranges them.
#[derive(Serialize)]
struct MonitorInfo {
    id: String,
    name: String,
    /// Top-left position in the global desktop space (can be negative).
    x: i32,
    y: i32,
    width: u32,
    height: u32,
    scale: f64,
}

/// The machine this shell runs on.
#[derive(Serialize)]
struct LocalDevice {
    id: String,
    name: String,
    os: String,
    monitors: Vec<MonitorInfo>,
}

/// A peer discovered on the LAN over mDNS (`_mouser._udp`), surfaced to the
/// Devices list. `host`/`port` are the first resolved address and the
/// interactive (`iport`) port — enough to show "who is out there", not to dial.
#[derive(Clone, Serialize)]
struct PeerInfo {
    id: String,
    name: String,
    os: String,
    host: String,
    port: u16,
}

/// Deduped set of LAN peers, keyed by DNS-SD instance fullname.
///
/// NOTE: This is a **UI-side mDNS shortcut**. The architecture's intended path
/// (docs/architecture.md §8) is for the engine to own discovery and hand peers
/// to the UI over `mouser-ipc`; the Tauri shell does not talk QUIC. Until that
/// wiring lands this browse loop makes real peers visible now without claiming
/// any trust/connection (TXT is advisory only, §4/§5).
///
/// The inner map is an `Arc<Mutex<…>>` so the background browse task can hold a
/// clone with a `'static` lifetime while the `discovered_peers` command reads
/// the same map through Tauri managed state.
#[derive(Clone, Default)]
struct DiscoveredPeers {
    /// `fullname` (the string `PeerEvent::Removed` carries) -> peer snapshot.
    by_fullname: Arc<Mutex<HashMap<String, PeerInfo>>>,
}

const MAIN_WINDOW_LABEL: &str = "main";
const TRAY_ID: &str = "mouser";
const TRAY_SHOW: &str = "show";
const TRAY_HIDE: &str = "hide";
const TRAY_QUIT: &str = "quit";

struct DesktopPreferences {
    tray_icon_visible: Mutex<bool>,
}

impl Default for DesktopPreferences {
    fn default() -> Self {
        Self {
            tray_icon_visible: Mutex::new(true),
        }
    }
}

fn lock_recover<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Returns the real local device: friendly name, OS, and the physical monitor
/// layout reported by the windowing system (positions and sizes converted from
/// physical pixels to logical points).
#[tauri::command]
fn local_device(window: tauri::Window) -> Result<LocalDevice, String> {
    let name = whoami::fallible::devicename()
        .or_else(|_| whoami::fallible::hostname())
        .unwrap_or_else(|_| "This computer".to_string());

    let monitors = window.available_monitors().map_err(|e| e.to_string())?;
    let mut out: Vec<MonitorInfo> = Vec::with_capacity(monitors.len());
    for (idx, m) in monitors.iter().enumerate() {
        let scale = m.scale_factor();
        let size = m.size();
        let pos = m.position();
        // Physical -> logical points so a 2x Retina panel and a 1x external
        // display share one coordinate space on the canvas.
        let width = ((f64::from(size.width)) / scale).round() as u32;
        let height = ((f64::from(size.height)) / scale).round() as u32;
        let x = ((f64::from(pos.x)) / scale).round() as i32;
        let y = ((f64::from(pos.y)) / scale).round() as i32;
        let display_name = m
            .name()
            .cloned()
            .unwrap_or_else(|| format!("Display {}", idx + 1));
        out.push(MonitorInfo {
            id: format!("local-mon-{idx}"),
            name: display_name,
            x,
            y,
            width,
            height,
            scale,
        });
    }

    Ok(LocalDevice {
        id: "local".to_string(),
        name,
        os: OS_KIND.to_string(),
        monitors: out,
    })
}

/// Snapshot of the LAN peers discovered so far, for the Devices list.
///
/// UI-side mDNS shortcut (see [`DiscoveredPeers`]): real peers become visible
/// here before the engine owns discovery over `mouser-ipc`.
#[tauri::command]
fn discovered_peers(state: tauri::State<'_, DiscoveredPeers>) -> Vec<PeerInfo> {
    lock_recover(&state.by_fullname)
        .values()
        .cloned()
        .collect()
}

/// Background mDNS browse loop: drives a [`Browser`] and folds `Found`/`Removed`
/// events into the shared [`DiscoveredPeers`] set. Runs for the app's lifetime
/// on the Tauri async runtime; returns (ending the loop) only if the browse
/// channel closes or the browser fails to start.
async fn run_discovery(peers: DiscoveredPeers) {
    let browser = match Browser::browse() {
        Ok(browser) => browser,
        // No mDNS daemon (no network / permissions): leave the set empty so the
        // UI honestly shows "no peers" rather than crashing the shell.
        Err(_) => return,
    };

    while let Some(event) = browser.next_event().await {
        match event {
            PeerEvent::Found(advert) => {
                // `addrs` is guaranteed non-empty by `from_service_info` (an
                // address-less peer is skipped upstream), but stay panic-free.
                let Some(host) = advert.addrs.first().map(|ip| ip.to_string()) else {
                    continue;
                };
                // Key by the DNS-SD fullname so `Removed` (which carries only the
                // fullname) can prune the exact entry.
                let fullname = format!("{}.{}", advert.instance_name(), mouser_net::SERVICE_TYPE);
                let info = PeerInfo {
                    id: advert.id,
                    name: advert.name,
                    os: advert.os,
                    host,
                    port: advert.iport,
                };
                lock_recover(&peers.by_fullname).insert(fullname, info);
            }
            PeerEvent::Removed(fullname) => {
                lock_recover(&peers.by_fullname).remove(&fullname);
            }
        }
    }
}

fn show_main_window(app: &tauri::AppHandle) {
    if let Some(window) = app.get_webview_window(MAIN_WINDOW_LABEL) {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}

fn hide_main_window(app: &tauri::AppHandle) {
    if let Some(window) = app.get_webview_window(MAIN_WINDOW_LABEL) {
        let _ = window.hide();
    }
}

fn apply_tray_icon_visibility(app: &tauri::AppHandle, visible: bool) -> Result<(), String> {
    if let Some(tray) = app.tray_by_id(TRAY_ID) {
        tray.set_visible(visible).map_err(|e| e.to_string())?;
    }

    if let Some(window) = app.get_webview_window(MAIN_WINDOW_LABEL) {
        window
            .set_skip_taskbar(visible)
            .map_err(|e| e.to_string())?;
        if !visible {
            let _ = window.show();
            let _ = window.unminimize();
        }
    }

    Ok(())
}

#[tauri::command]
fn set_tray_icon_visible(
    app: tauri::AppHandle,
    prefs: tauri::State<'_, DesktopPreferences>,
    visible: bool,
) -> Result<bool, String> {
    apply_tray_icon_visibility(&app, visible)?;
    *lock_recover(&prefs.tray_icon_visible) = visible;
    Ok(visible)
}

fn is_tray_icon_visible(prefs: &DesktopPreferences) -> bool {
    *lock_recover(&prefs.tray_icon_visible)
}

fn install_tray(app: &tauri::App) -> tauri::Result<()> {
    let menu = MenuBuilder::new(app)
        .text(TRAY_SHOW, "Show Mouser")
        .text(TRAY_HIDE, "Hide")
        .separator()
        .text(TRAY_QUIT, "Quit")
        .build()?;
    let icon = app.default_window_icon().cloned();

    let mut tray = TrayIconBuilder::with_id(TRAY_ID)
        .menu(&menu)
        .tooltip("Mouser")
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id().as_ref() {
            TRAY_SHOW => show_main_window(app),
            TRAY_HIDE => hide_main_window(app),
            TRAY_QUIT => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| match event {
            TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            }
            | TrayIconEvent::DoubleClick {
                button: MouseButton::Left,
                ..
            } => show_main_window(tray.app_handle()),
            _ => {}
        });

    if let Some(icon) = icon {
        tray = tray.icon(icon);
    }

    tray.build(app)?;
    Ok(())
}

/// Builds and runs the Tauri application.
///
/// Kept in the library (not `main.rs`) so the same entry point can be reused by
/// other shells/targets and exercised from `cargo build -p mouser-desktop`
/// without a `main` symbol clash on mobile.
#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .manage(DesktopPreferences::default())
        .manage(DiscoveredPeers::default())
        .setup(|app| {
            install_tray(app)?;
            let _ = apply_tray_icon_visibility(app.handle(), true);
            // Spawn the LAN mDNS browse loop (UI-side shortcut — see
            // `DiscoveredPeers`). Clone the managed `Arc` so the task holds a
            // `'static` handle to the same map the command reads.
            let peers = app.state::<DiscoveredPeers>().inner().clone();
            tauri::async_runtime::spawn(async move {
                run_discovery(peers).await;
            });
            Ok(())
        })
        .on_window_event(|window, event| {
            if window.label() == MAIN_WINDOW_LABEL {
                if let WindowEvent::CloseRequested { api, .. } = event {
                    let prefs = window.state::<DesktopPreferences>();
                    if is_tray_icon_visible(&prefs) {
                        api.prevent_close();
                        let _ = window.hide();
                    } else {
                        window.app_handle().exit(0);
                    }
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            local_device,
            discovered_peers,
            set_tray_icon_visible
        ])
        .run(tauri::generate_context!())
        .expect("error while running Mouser desktop shell");
}
