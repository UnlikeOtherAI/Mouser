//! The Mouser engine, run **in-process** by the desktop app — so the user never starts a
//! daemon by hand and there is no separate `mouserd` child to supervise. The app builds
//! the host's per-OS capture/injection/clipboard adapters, opens the daemon store, and
//! spawns [`mouser_engine::daemon::run_engine`] on the Tauri async runtime. Discovery,
//! trust, and the live connection all live in the engine; the UI reads its state over
//! `mouser_ipc` against the IPC server `run_engine` hosts on the well-known socket.
//!
//! Lifecycle is caller-owned: [`start`] spawns the engine task and stashes its
//! `JoinHandle` plus an [`InputCapture`] handle in Tauri-managed state; [`shutdown`]
//! aborts the task AND calls `capture.stop()` so any installed input hooks are released
//! on quit (the no-stuck-keys path).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::Mutex;

use mouser_core::platform::InputCapture;
use tauri::async_runtime::JoinHandle;
use tauri::Manager;

use crate::lock_recover;

/// Handle to the in-process engine task and its capture adapter, kept in Tauri-managed
/// state. On quit [`shutdown`] aborts the task and calls `capture.stop()` to release input
/// hooks. `None` when the engine could not be started (e.g. Linux `/dev/uinput` missing).
#[derive(Default)]
pub struct EngineHandle {
    inner: Mutex<Option<EngineRunning>>,
}

struct EngineRunning {
    task: JoinHandle<()>,
    capture: Arc<dyn InputCapture>,
}

/// Build the host's per-OS adapters, open the daemon store, and spawn the engine in-process
/// on the Tauri async runtime. Mirrors the adapter construction in
/// `crates/mouser-engine/src/bin/mouserd.rs`. The spawned task owns the store + adapter
/// `Arc`s (so it is `'static`); a clone of the capture adapter is retained in state so
/// [`shutdown`] can release input hooks on quit.
///
/// Engine startup failures are logged and skipped rather than fatal: the app stays alive in
/// the tray and the UI degrades to the "engine not running" hint (the IPC socket simply
/// never answers).
pub fn start(app: &tauri::AppHandle) {
    let store = match mouser_engine::daemon_store::DaemonStore::open_default() {
        Ok(store) => store,
        Err(e) => {
            mouser_engine::diag!(
                error,
                "mouser-desktop: cannot open engine store: {e}; engine not started"
            );
            return;
        }
    };

    #[cfg(target_os = "macos")]
    let adapters: Option<Adapters> = {
        let injector = Arc::new(platform_mac::adapter::MacInjector::new());
        let capture = Arc::new(platform_mac::adapter::MacCapture::new());
        let clipboard = Arc::new(platform_mac::MacClipboard::new());
        Some(Adapters {
            injector,
            capture,
            clipboard,
        })
    };

    #[cfg(target_os = "windows")]
    let adapters: Option<Adapters> = {
        let injector = Arc::new(platform_win::WinInjector::new());
        let capture = Arc::new(platform_win::WinCapture::new());
        let clipboard = Arc::new(platform_win::WinClipboard::new());
        Some(Adapters {
            injector,
            capture,
            clipboard,
        })
    };

    #[cfg(target_os = "linux")]
    let adapters: Option<Adapters> = {
        // `UinputInjector::new()` opens /dev/uinput and can fail (permissions). Log + skip
        // engine start rather than panicking — the app stays alive in the tray.
        match platform_linux::UinputInjector::new() {
            Ok(injector) => Some(Adapters {
                injector: Arc::new(injector),
                capture: Arc::new(platform_linux::LinuxCapture::new()),
                clipboard: Arc::new(platform_linux::LinuxClipboard::new()),
            }),
            Err(e) => {
                mouser_engine::diag!(
                    error,
                    "mouser-desktop: cannot open /dev/uinput ({e}); add the user to the \
                     `input` group (or run as root) and relaunch. Engine not started."
                );
                None
            }
        }
    };

    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    let adapters: Option<Adapters> = {
        mouser_engine::diag!(
            error,
            "mouser-desktop: this host's platform adapters are not wired into the engine \
             yet (macOS, Windows and Linux are supported). Engine not started."
        );
        None
    };

    let Some(adapters) = adapters else {
        return;
    };

    // Retain a capture handle for the shutdown path (release hooks on quit), then move the
    // OWNED store + adapter Arcs into the 'static task.
    let capture_for_shutdown = Arc::clone(&adapters.capture) as Arc<dyn InputCapture>;
    let Adapters {
        injector,
        capture,
        clipboard,
    } = adapters;

    // Run the engine in "target" role for the desktop UI: explicit-connect-only. The user
    // drives connections from the UI (an IPC Connect dials *and* controls the peer), and
    // inbound peers are still accepted — but the engine never AUTO-dials a discovered peer.
    // This makes Disconnect stick (in "auto" the lower-id side instantly re-dials a trusted
    // peer, undoing the disconnect) and avoids the reconnect-redial loop on a dropped link
    // (a "target" lost connection returns to discovery instead of redialing forever, which
    // otherwise shows as a stuck "connecting").
    let task = tauri::async_runtime::spawn(async move {
        if let Err(e) = mouser_engine::daemon::run_engine(
            store,
            "target".to_string(),
            injector,
            capture,
            clipboard,
        )
        .await
        {
            mouser_engine::diag!(error, "mouser-desktop: engine exited: {e}");
        }
    });

    let state = app.state::<EngineHandle>();
    *lock_recover(&state.inner) = Some(EngineRunning {
        task,
        capture: capture_for_shutdown,
    });
}

/// Per-OS adapter bundle handed to the in-process engine. The concrete types differ per
/// platform; the engine takes them as trait objects.
struct Adapters {
    #[cfg(target_os = "macos")]
    injector: Arc<platform_mac::adapter::MacInjector>,
    #[cfg(target_os = "macos")]
    capture: Arc<platform_mac::adapter::MacCapture>,
    #[cfg(target_os = "macos")]
    clipboard: Arc<platform_mac::MacClipboard>,

    #[cfg(target_os = "windows")]
    injector: Arc<platform_win::WinInjector>,
    #[cfg(target_os = "windows")]
    capture: Arc<platform_win::WinCapture>,
    #[cfg(target_os = "windows")]
    clipboard: Arc<platform_win::WinClipboard>,

    #[cfg(target_os = "linux")]
    injector: Arc<platform_linux::UinputInjector>,
    #[cfg(target_os = "linux")]
    capture: Arc<platform_linux::LinuxCapture>,
    #[cfg(target_os = "linux")]
    clipboard: Arc<platform_linux::LinuxClipboard>,
}

/// Stop the in-process engine on app exit: abort the serve task AND call `capture.stop()`
/// to release any installed input hooks. `capture.stop()` is the no-stuck-keys path — the
/// serve loop's own `ctrl_c` arms never fire in a windowed app, so the abort alone would
/// leave low-level hooks installed. Idempotent: a second call is a no-op.
pub fn shutdown(app: &tauri::AppHandle) {
    let state = app.state::<EngineHandle>();
    let running = lock_recover(&state.inner).take();
    if let Some(running) = running {
        // Release input hooks first (no stuck keys), then abort the serve task. `stop()` is
        // idempotent; we ignore its result since we're tearing down regardless.
        let _ = running.capture.stop();
        running.task.abort();
    }
}

/// Placeholder path kept for the existing Diagnostics command shim; the in-process
/// engine exposes diagnostics through [`mouser_engine::diagnostics`] instead of a file.
pub fn engine_log_path(_app: &tauri::AppHandle) -> Option<PathBuf> {
    Some(PathBuf::from("in-memory-engine-log"))
}

/// Read the in-memory engine log tail (up to `max_bytes`).
pub fn read_log_tail(_path: &Path, max_bytes: usize) -> Result<String, String> {
    Ok(mouser_engine::diagnostics::tail(max_bytes))
}
