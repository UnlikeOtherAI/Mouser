//! AppReveal integration — debug-only, macOS-only.
//!
//! AppReveal (github.com/UnlikeOtherAI/AppReveal) is a debug-only in-app MCP
//! server for native apps. A Tauri macOS app is a native AppKit + `WKWebView`
//! surface (tao/wry create the `NSApplication` / `NSWindow` / `WKWebView`), which
//! is exactly what AppReveal instruments on iOS/macOS — so starting it here makes
//! the live Mac window and its `WKWebView` inspectable/drivable over
//! `_appreveal._tcp`, matching the iOS/Android apps that already embed AppReveal.
//!
//! The Swift entry point lives in `appreveal-shim/` (a small SwiftPM package that
//! depends on AppReveal and exposes `@_cdecl("mouser_appreveal_start")`).
//! `build.rs` builds it into a self-contained dylib and links it in, **debug +
//! macOS only**. Release / non-macOS builds compile [`start`] to a no-op (see the
//! `cfg` below) and never link or depend on AppReveal.

/// Start the AppReveal in-app MCP server (debug macOS builds only).
///
/// On release builds, or any non-macOS target, this is a no-op: `build.rs` does
/// not link the shim there, so there is no symbol to call and no production
/// footprint. Call it once from the Tauri `setup()` hook, which runs on the AppKit
/// main thread (AppReveal's `start()` is `@MainActor`).
pub fn start() {
    #[cfg(all(debug_assertions, target_os = "macos"))]
    {
        // The only `unsafe` in this crate: a single FFI call into the AppReveal
        // Swift shim. The function takes no arguments, returns nothing, and hops
        // to the main actor internally, so there are no aliasing/lifetime concerns.
        #[allow(unsafe_code)]
        // SAFETY: `mouser_appreveal_start` is provided by libAppRevealShim.dylib,
        // linked + rpath-staged by build.rs for debug macOS builds. It is a
        // nullary, void Swift `@_cdecl` function that internally dispatches onto
        // the main actor; calling it is sound.
        unsafe {
            mouser_appreveal_start();
        }
    }
}

#[cfg(all(debug_assertions, target_os = "macos"))]
#[allow(unsafe_code)]
extern "C" {
    /// C-callable entry point from `appreveal-shim` that calls `AppReveal.start()`.
    fn mouser_appreveal_start();
}
