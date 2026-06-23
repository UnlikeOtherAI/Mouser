fn main() {
    // The app now runs the engine IN-PROCESS (no separate `mouserd` child), so there is
    // no sidecar to stage or bundle. The `stage_mouserd_sidecar` build step is gone.
    #[cfg(all(debug_assertions, target_os = "macos"))]
    link_appreveal_shim();
    tauri_build::build();
}

/// Build + link AppReveal (the debug-only in-app MCP server) into the Tauri
/// binary, **debug + macOS only**. A Tauri macOS app is a native
/// AppKit + WKWebView surface, so AppReveal can instrument the live window and
/// its `WKWebView` once `AppReveal.start()` is called from the Rust process.
///
/// `appreveal-shim/` is a small Swift package that depends on AppReveal and
/// exposes a C-callable `mouser_appreveal_start()` (see its `Shim.swift`). We
/// build it as a self-contained dylib (SwiftPM links AppReveal + the Swift
/// runtime stubs in) and link the binary against it with an `@loader_path` rpath,
/// then stage the dylib next to the output binary so `cargo run` / `tauri dev` /
/// the `--debug` bundle all resolve it at load time.
///
/// Non-fatal by design: if `swift` is missing or the build fails we emit a
/// `cargo:warning` and return — the app still builds and runs, just without
/// AppReveal. Release builds skip this entirely (`cfg(debug_assertions)`), so
/// there is zero production footprint and no production dependency.
#[cfg(all(debug_assertions, target_os = "macos"))]
fn link_appreveal_shim() {
    use std::path::PathBuf;
    use std::process::Command;

    let manifest_dir =
        PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap_or_else(|| ".".into()));
    let shim_dir = manifest_dir.join("appreveal-shim");

    // Rebuild if the shim sources or manifest change.
    println!(
        "cargo:rerun-if-changed={}",
        shim_dir.join("Package.swift").display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        shim_dir.join("Sources").display()
    );

    // AppReveal's API is `#if DEBUG`; build the shim in the debug configuration so
    // the symbols exist. `-Xswiftc -DDEBUG` makes the `#if DEBUG` blocks compile in.
    let status = Command::new("swift")
        .args([
            "build",
            "-c",
            "debug",
            "-Xswiftc",
            "-DDEBUG",
            "--package-path",
        ])
        .arg(&shim_dir)
        .status();

    let ok = match status {
        Ok(s) => s.success(),
        Err(e) => {
            println!(
                "cargo:warning=AppReveal shim: `swift build` could not run ({e}); \
                 building without in-app MCP server"
            );
            return;
        }
    };
    if !ok {
        println!(
            "cargo:warning=AppReveal shim: `swift build` failed; \
             building without in-app MCP server"
        );
        return;
    }

    // Ask SwiftPM where it put the artifacts rather than hardcoding the triple.
    let bin_path = Command::new("swift")
        .args(["build", "-c", "debug", "--show-bin-path", "--package-path"])
        .arg(&shim_dir)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| PathBuf::from(String::from_utf8_lossy(&o.stdout).trim().to_string()));

    let Some(bin_path) = bin_path else {
        println!(
            "cargo:warning=AppReveal shim: could not resolve SwiftPM bin path; \
             building without in-app MCP server"
        );
        return;
    };

    let dylib = bin_path.join("libAppRevealShim.dylib");
    if !dylib.exists() {
        println!(
            "cargo:warning=AppReveal shim dylib not found at {}; \
             building without in-app MCP server",
            dylib.display()
        );
        return;
    }

    // Stage the dylib next to the output binary so an `@loader_path` rpath resolves
    // it for `cargo run`, `tauri dev`, and the `tauri build --debug` app bundle.
    // OUT_DIR is `target/<profile>/build/<pkg>-<hash>/out`; the binary lands in
    // `target/<profile>/`, three levels up.
    if let Some(out_dir) = std::env::var_os("OUT_DIR").map(PathBuf::from) {
        if let Some(profile_dir) = out_dir
            .parent()
            .and_then(|p| p.parent())
            .and_then(|p| p.parent())
        {
            let staged = profile_dir.join("libAppRevealShim.dylib");
            if let Err(e) = std::fs::copy(&dylib, &staged) {
                println!(
                    "cargo:warning=AppReveal shim: failed to stage dylib to {}: {e}",
                    staged.display()
                );
            }
        }
    }

    println!("cargo:rustc-link-search=native={}", bin_path.display());
    println!("cargo:rustc-link-lib=dylib=AppRevealShim");
    // The binary loads the dylib from its own directory (`@loader_path`); the staged
    // copy above puts it there. The SwiftPM bin path is also on the rpath as a
    // fallback for plain `cargo build` runs that don't stage.
    println!("cargo:rustc-link-arg=-Wl,-rpath,@loader_path");
    println!("cargo:rustc-link-arg=-Wl,-rpath,{}", bin_path.display());
}
