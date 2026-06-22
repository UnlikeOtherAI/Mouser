//! The bundled `mouserd` engine the desktop app administers — so the user never starts a
//! daemon by hand. The app resolves the binary, launches it, **supervises** it (relaunch
//! if it exits), and stops it on quit. Discovery, trust, and the live connection all live
//! in the engine; the UI reads its state over `mouser_ipc`.

use std::fs::{self, File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::time::Duration;

#[cfg(windows)]
use std::os::windows::process::CommandExt;

use tauri::Manager;

use crate::lock_recover;

/// How often the supervisor checks the engine is reachable (and relaunches it if not).
const SUPERVISE_INTERVAL: Duration = Duration::from_secs(3);

/// Windows process-creation flags: run the engine with no console window and detached
/// from the app's console, so the bundled `mouserd` never flashes a terminal.
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
#[cfg(windows)]
const DETACHED_PROCESS: u32 = 0x0000_0008;

/// Handle to the `mouserd` engine the app launches and supervises (spawn on launch,
/// relaunch on crash, stop on quit). `None` when an engine was already running and we
/// attached to it instead of spawning our own.
#[derive(Default)]
pub struct EngineProcess {
    child: Mutex<Option<Child>>,
}

/// Resolve the bundled `mouserd` engine from the installed app resources, else fall back
/// to a `mouserd` on `PATH` (dev runs).
fn resolve_mouserd(app: &tauri::AppHandle) -> PathBuf {
    if let Ok(dir) = app.path().resource_dir() {
        let binaries = dir.join("binaries");
        let platform = binaries.join(mouserd_exe_name());
        if platform.exists() {
            return platform;
        }
        let extensionless = binaries.join("mouserd");
        if extensionless.exists() {
            return extensionless;
        }
    }
    PathBuf::from(mouserd_exe_name())
}

fn mouserd_exe_name() -> &'static str {
    if cfg!(windows) {
        "mouserd.exe"
    } else {
        "mouserd"
    }
}

fn mouserd_launch_args() -> &'static [&'static str] {
    // No explicit role on any platform: the daemon defaults to `auto` (advertise +
    // browse, either side can control). Windows used to be pinned to `target` because
    // becoming a source installed always-on low-level hooks that degraded local input;
    // capture is now ownership-driven (passive edge-sensing while idle, suppressing
    // hooks only while actively controlling), so Windows is a first-class controller.
    &[]
}

/// Supervise the engine: keep a `mouserd` reachable over IPC, relaunching the bundled
/// binary whenever it isn't (initial start, or after a crash). Runs until the app exits.
/// Windows starts in explicit receive-only target mode, avoiding global capture hooks
/// until the user explicitly connects.
pub fn supervise_engine(app: &tauri::AppHandle) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        loop {
            // An engine already answers (our child, a prior app instance, or a user-run
            // daemon) — nothing to do. Otherwise (re)launch the bundled engine.
            if mouser_ipc::Client::connect().await.is_err() {
                respawn_if_needed(&app);
            }
            tokio::time::sleep(SUPERVISE_INTERVAL).await;
        }
    });
}

/// Spawn the bundled engine if our previously-launched child is absent or has exited.
/// Never kills a child that is still running (it may just be slow to bind the IPC
/// socket), so a healthy-but-starting daemon is not double-spawned.
fn respawn_if_needed(app: &tauri::AppHandle) {
    let state = app.state::<EngineProcess>();
    {
        let mut guard = lock_recover(&state.child);
        if let Some(child) = guard.as_mut() {
            match child.try_wait() {
                Ok(Some(_)) | Err(_) => *guard = None, // exited (or unknown) → reap + respawn
                Ok(None) => return,                    // still running → don't double-spawn
            }
        }
    }
    let path = resolve_mouserd(app);
    let mut command = Command::new(&path);
    for arg in mouserd_launch_args() {
        command.arg(arg);
    }
    // The engine is a headless daemon; detach its stdio and (on Windows) suppress the
    // console window so it never flashes a terminal when (re)launched. Its stderr (the
    // daemon's own diagnostics: discovery, dials, trust checks, capture mode) is routed
    // to a log file the Diagnostics view can read, instead of being discarded.
    command.stdin(Stdio::null());
    command.stdout(Stdio::null());
    match engine_log_path(app).and_then(|p| open_log_file(&p)) {
        Some(file) => {
            command.stderr(Stdio::from(file));
        }
        None => {
            command.stderr(Stdio::null());
        }
    }
    #[cfg(windows)]
    command.creation_flags(CREATE_NO_WINDOW | DETACHED_PROCESS);
    match command.spawn() {
        Ok(child) => {
            *lock_recover(&state.child) = Some(child);
            eprintln!("mouser-desktop: launched engine {}", path.display());
        }
        Err(e) => eprintln!(
            "mouser-desktop: failed to launch engine {}: {e}",
            path.display()
        ),
    }
}

/// Path to the file the bundled engine's stderr is routed to (the daemon's own
/// diagnostics). `None` if the per-app log directory can't be resolved.
pub fn engine_log_path(app: &tauri::AppHandle) -> Option<PathBuf> {
    let dir = app.path().app_log_dir().ok()?;
    Some(dir.join("mouserd.log"))
}

/// Open the engine log file for the daemon's stderr, creating the directory and
/// truncating so each launch starts a fresh log. `None` on any I/O failure (then we
/// fall back to discarding stderr — logging is best-effort, never fatal).
fn open_log_file(path: &Path) -> Option<File> {
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(path)
        .ok()
}

/// Read the tail (up to `max_bytes`) of the engine log. Returns an empty string when
/// the log doesn't exist yet (engine not launched by us / just started).
pub fn read_log_tail(path: &Path, max_bytes: usize) -> Result<String, String> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(String::new()),
        Err(e) => return Err(e.to_string()),
    };
    let start = bytes.len().saturating_sub(max_bytes);
    Ok(String::from_utf8_lossy(&bytes[start..]).into_owned())
}

/// Stop the engine we launched (called on app exit) so we don't orphan the daemon.
pub fn stop_engine(app: &tauri::AppHandle) {
    let state = app.state::<EngineProcess>();
    let child = lock_recover(&state.child).take();
    if let Some(mut child) = child {
        let _ = child.kill();
        let _ = child.wait();
    }
}
