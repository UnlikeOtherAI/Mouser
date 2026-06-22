//! `mouserd` - the Mouser engine daemon (thin per-OS shim).
//!
//! All daemon logic lives in [`mouser_engine::daemon`]; this binary only selects the
//! host's concrete capture/injection adapters and hands them to [`mouser_engine::daemon::run`]:
//! - macOS   -> `platform-mac` (`MacCapture` + `MacInjector`),
//! - Windows -> `platform-win` (`WinCapture` + `WinInjector`),
//! - Linux   -> `platform-linux` (`LinuxCapture` + `UinputInjector`).
//!
//! Usage:
//! - `mouserd`          - auto on macOS/Linux; receive-only target mode on Windows.
//! - `mouserd auto`     - advertise + browse; either connected side can control.
//! - `mouserd source`   - controller-only connection: capture + dial a discovered peer.
//! - `mouserd target`   - receive-only connection: accept + inject, no input hooks.
//! - `mouserd connect <host:port> <peer-id>` - direct trusted controller connection.
//! - `mouserd probe <host:port>`   - handshake-only transport check, no capture/inject.
//! - `mouserd identity` - print this machine's persistent device id.
//! - `mouserd trust <peer-id>` / `mouserd trusted` - manage trusted peer pins.

fn main() {
    #[cfg(target_os = "macos")]
    {
        let injector = std::sync::Arc::new(platform_mac::adapter::MacInjector::new());
        let capture = platform_mac::adapter::MacCapture::new();
        mouser_engine::daemon::run(injector, Box::new(capture));
    }
    #[cfg(target_os = "linux")]
    {
        let injector = match platform_linux::UinputInjector::new() {
            Ok(inj) => std::sync::Arc::new(inj),
            Err(e) => {
                eprintln!(
                    "mouserd: cannot open /dev/uinput ({e}); add the user to the \
                     `input` group (or run as root) and relaunch"
                );
                std::process::exit(1);
            }
        };
        let capture = platform_linux::LinuxCapture::new();
        mouser_engine::daemon::run(injector, Box::new(capture));
    }
    #[cfg(target_os = "windows")]
    {
        let injector = std::sync::Arc::new(platform_win::WinInjector::new());
        let capture = platform_win::WinCapture::new();
        mouser_engine::daemon::run(injector, Box::new(capture));
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        eprintln!(
            "mouserd: this host's platform adapters are not wired into the daemon yet \
             (macOS, Windows and Linux are supported). The engine library is platform-agnostic."
        );
        std::process::exit(1);
    }
}
