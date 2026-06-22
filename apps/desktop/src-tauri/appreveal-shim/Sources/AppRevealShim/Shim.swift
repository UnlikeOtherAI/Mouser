import Foundation
#if DEBUG
import AppReveal
#endif

/// C-callable entry point invoked from the Tauri (Rust) `setup()` hook in debug
/// macOS builds — see `apps/desktop/src-tauri/src/appreveal.rs`.
///
/// Starts AppReveal's debug-only in-app MCP server, which advertises the live
/// `NSApplication` window + its `WKWebView` over `_appreveal._tcp` so an external
/// agent can inspect/drive the running Mac app (visual/DOM parity with the
/// iOS/Android apps that already embed AppReveal).
///
/// `AppReveal.start()` is `@MainActor`-isolated, so we hop onto the main actor.
/// Tauri calls this from `setup()`, which already runs on the AppKit main thread,
/// so the `assumeIsolated` fast path normally fires; the async hop is a safety net.
///
/// No-op in release builds: AppReveal's whole API is `#if DEBUG`, so this function
/// compiles to an empty body when the package is built without `-D DEBUG` (and
/// `build.rs` only ever links it in debug anyway).
@_cdecl("mouser_appreveal_start")
public func mouser_appreveal_start() {
    #if DEBUG
    if Thread.isMainThread {
        MainActor.assumeIsolated { AppReveal.start() }
    } else {
        DispatchQueue.main.async { AppReveal.start() }
    }
    #endif
}
