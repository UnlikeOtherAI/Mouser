// swift-tools-version: 5.9
//
// AppRevealShim — a tiny Swift bridge that lets the Tauri (Rust) desktop binary
// start AppReveal, the debug-only in-app MCP server (github.com/UnlikeOtherAI/AppReveal).
//
// A Tauri macOS app IS a native AppKit + WKWebView surface (tao/wry build the
// NSApplication / NSWindow / WKWebView), which is exactly what AppReveal
// instruments on iOS/macOS. The only missing piece is calling the Swift
// `AppReveal.start()` entry point from the Rust process — this package exposes a
// C-callable `mouser_appreveal_start()` (see Sources/AppRevealShim/Shim.swift)
// that `apps/desktop/src-tauri/build.rs` builds into a self-contained dylib and
// links into the binary, **debug + macOS only**. Release builds never see it.
//
// AppReveal's own `start()` API is wrapped in `#if DEBUG`, so this package is only
// ever built in the debug configuration (build.rs invokes `swift build -c debug`).
import PackageDescription

let package = Package(
    name: "AppRevealShim",
    platforms: [.macOS(.v13)],
    products: [
        // Dynamic so SwiftPM links AppReveal + the Swift runtime stubs into one
        // self-contained dylib the Rust binary can load via an @loader_path rpath.
        .library(name: "AppRevealShim", type: .dynamic, targets: ["AppRevealShim"]),
    ],
    dependencies: [
        .package(url: "https://github.com/UnlikeOtherAI/AppReveal.git", from: "0.9.8"),
    ],
    targets: [
        .target(name: "AppRevealShim", dependencies: ["AppReveal"]),
    ]
)
