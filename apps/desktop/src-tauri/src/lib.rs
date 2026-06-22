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

use serde::Serialize;

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

/// Builds and runs the Tauri application.
///
/// Kept in the library (not `main.rs`) so the same entry point can be reused by
/// other shells/targets and exercised from `cargo build -p mouser-desktop`
/// without a `main` symbol clash on mobile.
#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .invoke_handler(tauri::generate_handler![local_device])
        .run(tauri::generate_context!())
        .expect("error while running Mouser desktop shell");
}
