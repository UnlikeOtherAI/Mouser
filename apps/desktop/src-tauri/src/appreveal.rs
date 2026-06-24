//! AppReveal integration — debug-only; macOS (Swift shim) + Windows (Rust crate).
//!
//! AppReveal (github.com/UnlikeOtherAI/AppReveal) is a debug-only in-app MCP
//! server for native apps. It lets an external agent inspect/drive the running
//! app (window/state/DOM/diagnostics) over a local MCP endpoint — the iOS/Android
//! apps already embed it.
//!
//! A Tauri desktop app is a native window hosting a webview (AppKit + `WKWebView`
//! on macOS, Win32 + WebView2 on Windows), which is what AppReveal instruments. We
//! start it from the Tauri `setup()` hook, **debug builds only**, so release builds
//! never run it (the server also requires an explicit start + a per-session token).
//!
//! - **macOS**: the Swift entry point lives in `appreveal-shim/` (a SwiftPM package
//!   that depends on AppReveal and exposes `@_cdecl("mouser_appreveal_start")`);
//!   `build.rs` builds + links it, debug + macOS only. The call is the one `unsafe`
//!   FFI in this crate.
//! - **Windows**: the `appreveal-tauri` crate is a pure-Rust foundation that starts
//!   a loopback, token-guarded HTTP MCP server and derives window/device metadata
//!   from the `AppHandle` (no FFI, no `unsafe`).
//!
//! Release / unsupported targets compile [`start`] to a no-op.

/// Start the AppReveal in-app MCP server (debug builds only).
///
/// Call once from the Tauri `setup()` hook (the AppKit/Win32 main thread). On
/// release builds, or unsupported targets, this is a no-op with no production
/// footprint: macOS does not link the Swift shim there, and the Windows server is
/// gated behind `cfg(debug_assertions)`.
pub fn start(app: &tauri::AppHandle) {
    #[cfg(all(debug_assertions, target_os = "windows"))]
    {
        start_windows(app);
    }

    #[cfg(all(debug_assertions, target_os = "macos"))]
    {
        // The only `unsafe` in this crate: a single FFI call into the AppReveal
        // Swift shim. The function takes no arguments, returns nothing, and hops to
        // the main actor internally, so there are no aliasing/lifetime concerns. The
        // shim locates the live `NSApplication` itself, so it needs no handle.
        #[allow(unsafe_code)]
        // SAFETY: `mouser_appreveal_start` is provided by libAppRevealShim.dylib,
        // linked + rpath-staged by build.rs for debug macOS builds. It is a nullary,
        // void Swift `@_cdecl` function that internally dispatches onto the main
        // actor; calling it is sound.
        unsafe {
            mouser_appreveal_start();
        }
    }

    // `app` is consumed only on debug Windows; discard it elsewhere (release, macOS,
    // Linux) so the parameter never trips the unused-variable lint.
    #[cfg(not(all(debug_assertions, target_os = "windows")))]
    let _ = app;
}

/// Start AppReveal's `appreveal-tauri` MCP server on debug Windows builds.
///
/// The server is loopback-only and token-guarded; the printed `session_url`
/// carries the per-session token, which an agent on this machine uses to call the
/// MCP (`initialize` / `tools/list` / `tools/call`). Failures are logged and
/// swallowed — AppReveal is a dev aid and must never block app startup.
#[cfg(all(debug_assertions, target_os = "windows"))]
fn start_windows(app: &tauri::AppHandle) {
    use appreveal_tauri::{AppRevealTauriServer, ServerConfig};
    use tauri::Manager;

    match appreveal_tauri::start_tauri_server_managed(app.clone(), ServerConfig::localhost(0)) {
        Ok(addr) => match app.state::<AppRevealTauriServer>().session_url() {
            Ok(Some(url)) => {
                eprintln!("AppReveal: in-app MCP server listening at {url}");
                // Loopback + token, not mDNS-discoverable, and the port/token change on
                // every launch — persist the tokenized URL to a fixed temp file so the
                // user (or an agent on this machine) can find it without scraping stderr.
                let _ = std::fs::write(std::env::temp_dir().join("mouser-appreveal-url.txt"), &url);
            }
            Ok(None) => eprintln!("AppReveal: in-app MCP server started on {addr}"),
            Err(e) => eprintln!("AppReveal: started on {addr}, but session url unavailable: {e}"),
        },
        Err(e) => eprintln!("AppReveal: could not start in-app MCP server: {e}"),
    }
}

#[cfg(all(debug_assertions, target_os = "macos"))]
#[allow(unsafe_code)]
extern "C" {
    /// C-callable entry point from `appreveal-shim` that calls `AppReveal.start()`.
    fn mouser_appreveal_start();
}
