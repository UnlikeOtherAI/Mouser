//! Mouser desktop UI shell (Tauri v2).
//!
//! This crate is a **UI client only**. Per `docs/tech-stack.md` §5 and
//! `docs/architecture.md` §3/§8 it does NOT own the daemon lifecycle and does
//! NOT embed `mouser-core`. When wiring lands it will talk to the engine over
//! `mouser-ipc` (typed DTOs). For now this is a static shell with no backend
//! commands registered.

/// Builds and runs the Tauri application.
///
/// Kept in the library (not `main.rs`) so the same entry point can be reused by
/// other shells/targets and exercised from `cargo build -p mouser-desktop`
/// without a `main` symbol clash on mobile.
#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .run(tauri::generate_context!())
        .expect("error while running Mouser desktop shell");
}
