//! Mouser desktop UI shell (Tauri v2).
//!
//! Per `docs/tech-stack.md` §5 and `docs/architecture.md` §3/§8 this crate does NOT own
//! the daemon lifecycle and does NOT embed `mouser-core`. It talks to the headless
//! `mouserd` engine over [`mouser_ipc`] (typed DTOs over a local Unix-domain socket):
//! the engine owns discovery, trust, and the live connection; this shell reflects that
//! state ([`engine_snapshot`]) and drives it ([`connect_peer`] / [`disconnect_peer`]).
//!
//! [`local_device`] still surfaces the **real local machine** (name, OS, physical
//! display arrangement) directly from the windowing system, since the engine snapshot
//! describes peers, not this machine's monitors. When the daemon is not running the
//! snapshot command reports `engine_running: false` so the UI can degrade gracefully
//! (show the local device + an "engine not running" hint).

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Child;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use mouser_ipc::{Client, Command, Snapshot};
use mouser_net::{Browser, PeerAdvert, PeerEvent};
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

/// How long a `connect_peer`/`disconnect_peer` command may take before giving up.
const COMMAND_TIMEOUT: Duration = Duration::from_secs(3);

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

/// The engine's live state, plus whether the daemon is reachable, surfaced to the UI.
///
/// Mirrors [`mouser_ipc::Snapshot`] but with a JSON-friendly connection shape and the
/// `engine_running` flag so the frontend can show an honest "engine not running" hint.
#[derive(Serialize)]
struct EngineSnapshot {
    /// True when the daemon's IPC socket answered; false means it is not running.
    engine_running: bool,
    /// Peers the engine has discovered, with trust + connection-relevant fields.
    peers: Vec<EnginePeer>,
    /// Current connection state.
    connection: EngineConnection,
}

/// A peer as the engine reports it (mirrors [`mouser_ipc::PeerDto`]).
#[derive(Serialize)]
struct EnginePeer {
    id: String,
    name: String,
    os: String,
    host: String,
    port: u16,
    trusted: bool,
}

/// The engine connection state for the UI (mirrors [`mouser_ipc::ConnectionDto`]).
#[derive(Serialize)]
struct EngineConnection {
    /// `"idle" | "connecting" | "connected"`.
    state: String,
    peer_id: Option<String>,
    owner: Option<String>,
    epoch: Option<u64>,
}

impl EngineSnapshot {
    /// The daemon is not running, but we discovered peers directly over mDNS. Show them
    /// (read-only — `engine_running: false` tells the UI to prompt starting the engine
    /// before connecting). `trusted` is unknown without the engine, so reported `false`.
    fn from_mdns(adverts: Vec<PeerAdvert>) -> Self {
        let mut peers: Vec<EnginePeer> = adverts
            .into_iter()
            .map(|a| {
                let host = a.addrs.first().map(|ip| ip.to_string()).unwrap_or_default();
                EnginePeer {
                    name: if a.name.is_empty() {
                        host.clone()
                    } else {
                        a.name
                    },
                    id: a.id,
                    os: a.os,
                    host,
                    port: a.iport,
                    trusted: false,
                }
            })
            .collect();
        peers.sort_by(|a, b| a.id.cmp(&b.id));
        Self {
            engine_running: false,
            peers,
            connection: EngineConnection {
                state: "idle".to_string(),
                peer_id: None,
                owner: None,
                epoch: None,
            },
        }
    }

    /// Convert a live engine [`Snapshot`] into the UI shape.
    fn from_snapshot(snapshot: Snapshot) -> Self {
        Self {
            engine_running: true,
            peers: snapshot
                .peers
                .into_iter()
                .map(|p| EnginePeer {
                    id: p.id,
                    name: p.name,
                    os: p.os,
                    host: p.host,
                    port: p.port,
                    trusted: p.trusted,
                })
                .collect(),
            connection: EngineConnection {
                state: snapshot.connection.state.as_str().to_string(),
                peer_id: snapshot.connection.peer_id,
                owner: snapshot.connection.owner,
                epoch: snapshot.connection.epoch,
            },
        }
    }
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

/// Handle to the `mouserd` engine the app launches and supervises, so the user never
/// has to start a daemon themselves — the app administers it (spawn on launch, stop on
/// quit). `None` when an engine was already running and we attached to it instead.
#[derive(Default)]
struct EngineProcess {
    child: Mutex<Option<Child>>,
}

/// Resolve the bundled `mouserd` engine from the installed app resources, else fall
/// back to a `mouserd` on `PATH` (dev runs).
fn resolve_mouserd(app: &tauri::AppHandle) -> PathBuf {
    if let Ok(dir) = app.path().resource_dir() {
        let binaries = dir.join("binaries");
        let platform = binaries.join(mouserd_exe_name());
        if platform.exists() {
            return platform;
        }
        let extensionless = binaries.join("mouserd");
        if extensionless.exists() {
            return extensionless;
        }
    }
    PathBuf::from(mouserd_exe_name())
}

fn mouserd_exe_name() -> &'static str {
    if cfg!(windows) {
        "mouserd.exe"
    } else {
        "mouserd"
    }
}

/// Ensure the engine is running: if the IPC socket doesn't already answer, launch the
/// bundled `mouserd` and keep its handle so [`run`] can stop it on exit.
fn ensure_engine_running(app: &tauri::AppHandle) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        // An engine is already up (a prior app instance, or a user-run daemon) — attach.
        if mouser_ipc::Client::connect().await.is_ok() {
            return;
        }
        let path = resolve_mouserd(&app);
        let mut command = std::process::Command::new(&path);
        match command.spawn() {
            Ok(child) => {
                *lock_recover(&app.state::<EngineProcess>().child) = Some(child);
                eprintln!("mouser-desktop: launched engine {}", path.display());
            }
            Err(e) => eprintln!(
                "mouser-desktop: failed to launch engine {}: {e}",
                path.display()
            ),
        }
    });
}

/// Peers this app discovered directly over mDNS, keyed by DNS-SD instance fullname.
/// A standalone fallback so the Devices list is populated even when the headless
/// `mouserd` engine isn't running (discovery needs no engine).
#[derive(Default)]
struct DiscoveredPeers {
    inner: Arc<Mutex<HashMap<String, PeerAdvert>>>,
}

/// Snapshot the current mDNS-discovered peers.
fn mdns_peers(state: &DiscoveredPeers) -> Vec<PeerAdvert> {
    lock_recover(&state.inner).values().cloned().collect()
}

/// Continuously browse `_mouser._udp` and fold Found/Removed into the registry.
async fn mdns_browse_loop(inner: Arc<Mutex<HashMap<String, PeerAdvert>>>) {
    let browser = match Browser::browse() {
        Ok(b) => b,
        Err(_) => return, // no mDNS daemon available; leave the registry empty
    };
    while let Some(event) = browser.next_event().await {
        match event {
            PeerEvent::Found(advert) => {
                let key = format!("{}.{}", advert.instance_name(), mouser_net::SERVICE_TYPE);
                lock_recover(&inner).insert(key, advert);
            }
            PeerEvent::Removed(fullname) => {
                lock_recover(&inner).remove(&fullname);
            }
        }
    }
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

/// The engine's live snapshot (discovered peers + trust + connection state), fetched
/// over the local IPC link. If the daemon is not running this returns an offline
/// snapshot (`engine_running: false`) rather than failing, so the UI can show the
/// local device with an "engine not running" hint.
#[tauri::command]
async fn engine_snapshot(
    peers: tauri::State<'_, DiscoveredPeers>,
) -> Result<EngineSnapshot, String> {
    Ok(match fetch_engine_snapshot().await {
        // Engine is up: it owns discovery + trust + the live connection.
        Ok(snapshot) => EngineSnapshot::from_snapshot(snapshot),
        // Engine is down: fall back to the peers we discovered over mDNS ourselves, so
        // the Devices list is still populated (read-only until the engine is started).
        Err(_) => EngineSnapshot::from_mdns(mdns_peers(&peers)),
    })
}

/// Ask the engine to connect to a discovered, trusted peer by its base32 id.
#[tauri::command]
async fn connect_peer(peer_id: String) -> Result<(), String> {
    send_command(Command::Connect { peer_id }).await
}

/// Ask the engine to tear down the current connection.
#[tauri::command]
async fn disconnect_peer() -> Result<(), String> {
    send_command(Command::Disconnect).await
}

/// Open a short-lived IPC client, fetch one snapshot, and close. Commands are rare and
/// the snapshot is small, so a fresh connection per call keeps the shell stateless and
/// avoids a background reader task fighting the command path over one socket.
async fn fetch_engine_snapshot() -> Result<Snapshot, String> {
    let mut client = Client::connect().await.map_err(|e| e.to_string())?;
    tokio::time::timeout(COMMAND_TIMEOUT, client.fetch_snapshot())
        .await
        .map_err(|_| "engine did not reply in time".to_string())?
        .map_err(|e| e.to_string())
}

/// Open a short-lived IPC client and send one command.
async fn send_command(command: Command) -> Result<(), String> {
    let mut client = Client::connect().await.map_err(|e| e.to_string())?;
    tokio::time::timeout(COMMAND_TIMEOUT, client.send_command(&command))
        .await
        .map_err(|_| "engine did not accept the command in time".to_string())?
        .map_err(|e| e.to_string())
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
    let app = tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        // Cross-platform "Launch at login": the plugin manages the macOS LaunchAgent,
        // the Windows registry Run key, and the Linux XDG autostart entry. The General
        // settings toggle drives it over the JS API (enable/disable/isEnabled).
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            None,
        ))
        .manage(DesktopPreferences::default())
        .manage(DiscoveredPeers::default())
        .manage(EngineProcess::default())
        .setup(|app| {
            install_tray(app)?;
            let _ = apply_tray_icon_visibility(app.handle(), true);
            // The app administers the engine: launch the headless `mouserd` daemon if it
            // isn't already running, so the user never starts a daemon by hand.
            ensure_engine_running(app.handle());
            // Browse mDNS directly so the Devices list is populated even before the
            // engine's snapshot arrives (the engine snapshot takes over once it is up).
            let peers = app.state::<DiscoveredPeers>().inner.clone();
            tauri::async_runtime::spawn(mdns_browse_loop(peers));
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
            engine_snapshot,
            connect_peer,
            disconnect_peer,
            set_tray_icon_visible
        ])
        .build(tauri::generate_context!())
        .expect("error while building Mouser desktop shell");

    // Stop the engine we launched when the app exits, so we don't orphan the daemon.
    app.run(|app_handle, event| {
        if let tauri::RunEvent::Exit = event {
            if let Some(mut child) = lock_recover(&app_handle.state::<EngineProcess>().child).take()
            {
                let _ = child.kill();
            }
        }
    });
}
