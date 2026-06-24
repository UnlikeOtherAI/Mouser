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
//!
//! This crate can't adopt `[lints] workspace = true`: the debug macOS build links
//! the AppReveal Swift shim ([`appreveal`]) and must call its `extern "C"` entry
//! point — an `unsafe` FFI call — and the workspace `forbid`s `unsafe_code`
//! (`forbid` can't be locally relaxed). Instead it `#![deny(unsafe_code)]`
//! crate-wide (so every other path stays unsafe-free) and the `appreveal` module
//! locally `#[allow(unsafe_code)]` for the single FFI call. The workspace
//! panic-free clippy denies are replicated here too. Same pattern as platform-mac.

#![deny(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]

use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use mouser_ipc::{Client, Command, HealthItemDto, SettingsDto, Snapshot};
use serde::Serialize;
use tauri::{
    menu::MenuBuilder,
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    Manager, WindowEvent,
};

mod appreveal;
mod engine;
use engine::EngineHandle;

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
    /// This machine's engine device id (base32). The pairing id another device must
    /// trust to allow a connection — surfaced so the UI can show it for pairing.
    local_id: Option<String>,
    /// Peers the engine has discovered, with trust + connection-relevant fields.
    peers: Vec<EnginePeer>,
    /// Current connection state.
    connection: EngineConnection,
    /// A pending inbound pairing request awaiting Approve/Deny, if any.
    pairing: Option<EnginePairing>,
    /// Daemon-owned settings (input/clipboard/security) the UI reads + edits.
    settings: SettingsDto,
    /// Connectivity/permission health the engine detected (empty = healthy).
    diagnostics: Vec<HealthItemDto>,
}

/// A pending inbound pairing request (mirrors [`mouser_ipc::PairingDto`]).
#[derive(Serialize)]
struct EnginePairing {
    /// Base32 device id of the peer requesting control.
    peer_id: String,
    /// The peer's announced display name (or a generic fallback).
    name: String,
    /// Six decimal SAS digits to compare before approval.
    sas: String,
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
    /// Why the last connection attempt failed, when known (so the UI can explain it).
    error: Option<String>,
}

impl EngineSnapshot {
    /// The daemon's IPC socket did not answer. The engine owns discovery, so without it
    /// there are no peers to show; `engine_running: false` tells the UI to surface the
    /// "engine not running" hint. The desktop deliberately does **not** run its own mDNS
    /// browse — a second browsing daemon on the host races the engine's for inbound
    /// multicast and makes both silently miss peers (macOS).
    fn offline() -> Self {
        Self {
            engine_running: false,
            local_id: None,
            peers: Vec::new(),
            connection: EngineConnection {
                state: "idle".to_string(),
                peer_id: None,
                owner: None,
                epoch: None,
                error: None,
            },
            pairing: None,
            settings: SettingsDto::default(),
            diagnostics: Vec::new(),
        }
    }

    /// Convert a live engine [`Snapshot`] into the UI shape.
    fn from_snapshot(snapshot: Snapshot) -> Self {
        let local_id = Some(snapshot.local.id.clone());
        Self {
            engine_running: true,
            local_id,
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
                error: snapshot.connection.error,
            },
            pairing: snapshot.pairing.map(|p| EnginePairing {
                peer_id: p.peer_id,
                name: p.name,
                sas: p.sas,
            }),
            settings: snapshot.settings,
            diagnostics: snapshot.diagnostics,
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

pub(crate) fn lock_recover<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
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

/// The engine's live snapshot (discovered peers + trust + connection state), fetched
/// over the local IPC link. If the daemon is not running this returns an offline
/// snapshot (`engine_running: false`) rather than failing, so the UI can show the
/// local device with an "engine not running" hint.
#[tauri::command]
async fn engine_snapshot() -> Result<EngineSnapshot, String> {
    Ok(match fetch_engine_snapshot().await {
        // Engine is up: it owns discovery + trust + the live connection.
        Ok(snapshot) => EngineSnapshot::from_snapshot(snapshot),
        // Engine is down: report offline so the UI shows the "engine not running" hint.
        // The app supervises the engine, so this is only the brief startup window.
        Err(_) => EngineSnapshot::offline(),
    })
}

/// Ask the engine to connect to a discovered, trusted peer by its base32 id. The engine
/// owns discovery and resolves the address from its own registry, so the desktop supplies
/// only the peer id.
#[tauri::command]
async fn connect_peer(peer_id: String) -> Result<(), String> {
    send_command(Command::Connect { peer_id }).await
}

/// Ask the engine to tear down the current connection.
#[tauri::command]
async fn disconnect_peer() -> Result<(), String> {
    send_command(Command::Disconnect).await
}

/// Pair (trust) a discovered peer on this machine by its base32 id, so the engine
/// will allow a connection to/from it. Routes through the running daemon so it updates
/// its trust store AND republishes a fresh snapshot. NOTE: pairing is mutual — the
/// *other* device must also trust this machine's id before a connection forms (spec §3).
#[tauri::command]
async fn trust_peer(peer_id: String) -> Result<(), String> {
    send_command(Command::Trust { peer_id }).await
}

/// Read the tail of the engine (`mouserd`) log — the daemon's own diagnostics
/// (discovery, dials, trust checks, capture mode) — for the Diagnostics view.
#[tauri::command]
async fn engine_log(app: tauri::AppHandle) -> Result<String, String> {
    let path = engine::engine_log_path(&app).ok_or("engine log directory unavailable")?;
    tauri::async_runtime::spawn_blocking(move || engine::read_log_tail(&path, 128 * 1024))
        .await
        .map_err(|e| e.to_string())?
}

/// Approve a pending inbound pairing request: the engine trusts the peer and accepts its
/// (held-open) connection. `peer_id` is the base32 id from the pairing prompt.
#[tauri::command]
async fn approve_pairing(peer_id: String) -> Result<(), String> {
    send_command(Command::ApprovePairing { peer_id }).await
}

/// Deny a pending inbound pairing request: the engine closes the connection without
/// trusting the peer.
#[tauri::command]
async fn deny_pairing(peer_id: String) -> Result<(), String> {
    send_command(Command::DenyPairing { peer_id }).await
}

/// Replace the daemon's persisted settings. The UI sends the full settings struct
/// (current values with the changed field applied); the daemon saves + republishes,
/// so the next snapshot poll reflects it — and the MCP server sees the same state.
#[tauri::command]
async fn set_settings(settings: SettingsDto) -> Result<(), String> {
    send_command(Command::UpdateSettings { settings }).await
}

/// Forget all remembered state — clear trusted peers + restore default settings (the
/// device's identity is kept). The engine wipes its store and republishes a snapshot.
#[tauri::command]
async fn reset_data() -> Result<(), String> {
    send_command(Command::ResetData).await
}

/// Apply a connectivity remediation flagged by the engine's diagnostics: open the OS
/// settings pane where the user can fix it (e.g. Network Connections to disable a dead
/// adapter, or the firewall). Mapped per-OS; an unknown action is a no-op error.
#[tauri::command]
fn run_remediation(action: String) -> Result<(), String> {
    let (program, args) = match action.as_str() {
        "open_network_settings" => network_settings_command(),
        "check_firewall" => firewall_settings_command(),
        other => return Err(format!("unknown remediation action: {other}")),
    };
    std::process::Command::new(program)
        .args(&args)
        .spawn()
        .map(|_child| ())
        .map_err(|e| format!("could not open settings for {action}: {e}"))
}

#[cfg(target_os = "windows")]
fn network_settings_command() -> (&'static str, Vec<&'static str>) {
    // Network Connections (ncpa.cpl) — where a dead/disconnected adapter is disabled.
    ("control", vec!["ncpa.cpl"])
}
#[cfg(target_os = "windows")]
fn firewall_settings_command() -> (&'static str, Vec<&'static str>) {
    ("control", vec!["firewall.cpl"])
}
#[cfg(target_os = "macos")]
fn network_settings_command() -> (&'static str, Vec<&'static str>) {
    (
        "open",
        vec!["x-apple.systempreferences:com.apple.Network-Settings.extension"],
    )
}
#[cfg(target_os = "macos")]
fn firewall_settings_command() -> (&'static str, Vec<&'static str>) {
    (
        "open",
        vec!["x-apple.systempreferences:com.apple.settings.PrivacySecurity.extension"],
    )
}
#[cfg(target_os = "linux")]
fn network_settings_command() -> (&'static str, Vec<&'static str>) {
    // Best-effort: the exact pane varies by desktop environment.
    ("xdg-open", vec!["settings:///network"])
}
#[cfg(target_os = "linux")]
fn firewall_settings_command() -> (&'static str, Vec<&'static str>) {
    ("xdg-open", vec!["settings:///network"])
}

/// Input-permission status for the controlling machine. On macOS, capturing/forwarding
/// input needs **Accessibility** (suppress + inject) and **Input Monitoring** (the edge
/// tap); without them the cursor silently won't cross. `relevant` is false on platforms
/// that don't gate this behind a queryable runtime grant.
#[derive(serde::Serialize)]
struct InputPermissions {
    relevant: bool,
    accessibility: bool,
    input_monitoring: bool,
}

/// Report whether this machine has the grants needed to control a peer (drive its cursor).
#[tauri::command]
fn input_permissions() -> InputPermissions {
    #[cfg(target_os = "macos")]
    {
        InputPermissions {
            relevant: true,
            accessibility: platform_mac::accessibility_trusted(),
            input_monitoring: platform_mac::input_monitoring_trusted(),
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        // Windows (SetWindowsHookEx) and Linux (evdev) aren't gated by a runtime grant the
        // app can query, so report all-clear.
        InputPermissions {
            relevant: false,
            accessibility: true,
            input_monitoring: true,
        }
    }
}

/// Trigger the OS permission prompt for `kind` ("accessibility" | "input_monitoring") and
/// open the exact Privacy pane (the system prompt only fires once per launch, so we also
/// deep-link the user straight to the toggle). No-op on platforms without these grants.
#[tauri::command]
fn request_input_permission(kind: String) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        match kind.as_str() {
            "accessibility" => {
                let _ = platform_mac::prompt_accessibility();
                open_settings_url(
                    "x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility",
                )
            }
            "input_monitoring" => {
                let _ = platform_mac::prompt_input_monitoring();
                open_settings_url(
                    "x-apple.systempreferences:com.apple.preference.security?Privacy_ListenEvent",
                )
            }
            other => Err(format!("unknown permission kind: {other}")),
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = kind;
        Ok(())
    }
}

#[cfg(target_os = "macos")]
fn open_settings_url(url: &str) -> Result<(), String> {
    std::process::Command::new("open")
        .arg(url)
        .spawn()
        .map(|_child| ())
        .map_err(|e| format!("could not open settings: {e}"))
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

    // macOS: a tray-only app is a menu-bar agent — drop it from the Dock and the
    // Cmd-Tab app switcher (Accessory). Restore the Dock/switcher icon (Regular) in
    // taskbar/dock mode. skip_taskbar alone doesn't remove the Dock icon on macOS.
    #[cfg(target_os = "macos")]
    {
        let policy = if visible {
            tauri::ActivationPolicy::Accessory
        } else {
            tauri::ActivationPolicy::Regular
        };
        app.set_activation_policy(policy)
            .map_err(|e| e.to_string())?;
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
    // Menu-bar icon: the Font Awesome cursor (arrow-pointer), as a macOS template
    // image so it auto-tints to the light/dark menu bar. Falls back to the app icon.
    let icon = tray_cursor_icon().or_else(|| app.default_window_icon().cloned());

    let mut tray = TrayIconBuilder::with_id(TRAY_ID)
        .menu(&menu)
        .tooltip("Mouser")
        .icon_as_template(true)
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

/// The bundled Font Awesome cursor as a Tauri image for the menu-bar tray icon.
fn tray_cursor_icon() -> Option<tauri::image::Image<'static>> {
    const PNG: &[u8] = include_bytes!("../icons/tray-cursor.png");
    tauri::image::Image::from_bytes(PNG).ok()
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
        .manage(EngineHandle::default())
        .setup(|app| {
            install_tray(app)?;
            let _ = apply_tray_icon_visibility(app.handle(), true);
            // Debug builds only: start AppReveal, the in-app MCP server, so the live
            // window + its webview are inspectable/drivable by an external agent —
            // macOS via the Swift shim, Windows via the `appreveal-tauri` Rust crate
            // (loopback + token). No-op on release / unsupported targets — see [`appreveal`].
            appreveal::start(app.handle());
            // The app IS the engine: it runs the engine IN-PROCESS (no separate `mouserd`
            // child), so the user never starts a daemon by hand. `start` builds the host's
            // per-OS adapters, opens the daemon store, and spawns `run_engine` on the Tauri
            // async runtime, hosting the IPC server the #[tauri::command]s talk to. The
            // engine owns mDNS discovery; the app reads peers from its IPC snapshot rather
            // than running a second browse (which would race the engine's and miss peers).
            engine::start(app.handle());
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
            trust_peer,
            engine_log,
            approve_pairing,
            deny_pairing,
            set_settings,
            run_remediation,
            input_permissions,
            request_input_permission,
            reset_data,
            set_tray_icon_visible
        ])
        .build(tauri::generate_context!())
        .expect("error while building Mouser desktop shell");

    // Stop the in-process engine when the app exits: abort the serve task AND call
    // `capture.stop()` to release any installed input hooks (the no-stuck-keys path).
    app.run(|app_handle, event| {
        if let tauri::RunEvent::Exit = event {
            engine::shutdown(app_handle);
        }
    });
}
