use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;
use std::time::Duration;

use mouser_core::platform::{
    CaptureMode, InputCapture, InputSink, LocalInputEvent, PlatformError, PlatformResult,
};
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, GetMessageW, PeekMessageW, PostThreadMessageW, SetWindowsHookExW,
    UnhookWindowsHookEx, MSG, PM_NOREMOVE, WH_KEYBOARD_LL, WH_MOUSE_LL, WM_QUIT,
};

use crate::adapter::active_display_bounds;
use crate::capture_hooks::{
    capture_worker, clear_capture_state, keyboard_hook, mouse_hook, prepare_capture_state,
    stop_capture_worker, virtual_point_to_event,
};
use crate::capture_rawinput::{self, set_raw_input_active};
use crate::inject;

const PASSIVE_POLL_INTERVAL: Duration = Duration::from_millis(8);
const LEFT_CTRL_BIT: u16 = 1 << 0;
const RIGHT_CTRL_BIT: u16 = 1 << 4;
const EMERGENCY_RECLAIM_CTRL_MASK: u16 = LEFT_CTRL_BIT | RIGHT_CTRL_BIT;

/// The local emergency-reclaim chord is both Ctrl keys held at once.
#[must_use]
pub fn emergency_reclaim_chord_from_mods(mods: u16) -> bool {
    mods & EMERGENCY_RECLAIM_CTRL_MASK == EMERGENCY_RECLAIM_CTRL_MASK
}

/// Whether a captured key event exposes the emergency-reclaim chord to the sink.
#[must_use]
pub fn is_emergency_reclaim_event(event: LocalInputEvent) -> bool {
    matches!(
        event,
        LocalInputEvent::Key {
            down: true,
            mods,
            ..
        } if emergency_reclaim_chord_from_mods(mods)
    )
}

/// Windows input capture: a [`CaptureMode`] state machine over a passive cursor
/// poll (PassiveEdge) and the low-level `WH_*_LL` hooks (ActiveForward).
pub struct WinCapture {
    inner: Arc<Mutex<CaptureRun>>,
}

struct CaptureRun {
    mode: CaptureMode,
    hook_thread_id: Option<u32>,
    hook_handle: Option<JoinHandle<()>>,
    can_suppress: bool,
    passive_stop: Option<Arc<AtomicBool>>,
    passive_handle: Option<JoinHandle<()>>,
}

impl Default for WinCapture {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for WinCapture {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

impl WinCapture {
    /// A not-yet-started Windows capture handle (mode [`CaptureMode::Off`]).
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(CaptureRun {
                mode: CaptureMode::Off,
                hook_thread_id: None,
                hook_handle: None,
                can_suppress: false,
                passive_stop: None,
                passive_handle: None,
            })),
        }
    }
}

fn boxed<E: std::error::Error + Send + Sync + 'static>(e: E) -> PlatformError {
    Box::new(e)
}

fn lock_recover<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

fn teardown(run: &mut CaptureRun) {
    if let Some(stop) = run.passive_stop.take() {
        stop.store(true, Ordering::Release);
    }
    if let Some(handle) = run.passive_handle.take() {
        let _ = handle.join();
    }

    let hook_tid = run.hook_thread_id.take();
    if let Some(tid) = hook_tid {
        let _ = unsafe { PostThreadMessageW(tid, WM_QUIT, WPARAM(0), LPARAM(0)) };
    }
    if let Some(handle) = run.hook_handle.take() {
        let safe = hook_tid.is_none_or(|tid| unsafe { GetCurrentThreadId() } != tid);
        if safe {
            let _ = handle.join();
        }
    }

    run.can_suppress = false;
    run.mode = CaptureMode::Off;
}

fn start_passive_poll(
    sink: Arc<dyn InputSink>,
) -> PlatformResult<(Arc<AtomicBool>, JoinHandle<()>)> {
    let stop = Arc::new(AtomicBool::new(false));
    let stop_for_thread = Arc::clone(&stop);
    let handle = std::thread::Builder::new()
        .name("mouser-win-edge".into())
        .spawn(move || passive_poll_thread(sink, stop_for_thread))
        .map_err(boxed)?;
    Ok((stop, handle))
}

fn start_active_hooks(sink: Arc<dyn InputSink>) -> PlatformResult<(u32, JoinHandle<()>)> {
    prepare_capture_state(sink).map_err(boxed)?;
    let (tx, rx) = std::sync::mpsc::channel::<Result<u32, String>>();
    let handle = match std::thread::Builder::new()
        .name("mouser-win-capture".into())
        .spawn(move || run_capture_thread(tx))
    {
        Ok(handle) => handle,
        Err(e) => {
            clear_capture_state();
            return Err(boxed(e));
        }
    };

    match rx.recv() {
        Ok(Ok(thread_id)) => Ok((thread_id, handle)),
        Ok(Err(reason)) => {
            clear_capture_state();
            let _ = handle.join();
            Err(boxed(CaptureStartFailed(reason)))
        }
        Err(_) => {
            clear_capture_state();
            let _ = handle.join();
            Err(boxed(CaptureStartFailed(
                "capture thread exited before installing hooks".to_owned(),
            )))
        }
    }
}

impl InputCapture for WinCapture {
    fn set_mode(&self, mode: CaptureMode, sink: &Arc<dyn InputSink>) -> PlatformResult<()> {
        let mut run = lock_recover(&self.inner);
        if run.mode == mode {
            return Ok(());
        }
        teardown(&mut run);
        match mode {
            CaptureMode::Off => {}
            CaptureMode::PassiveEdge => {
                let (stop, handle) = start_passive_poll(Arc::clone(sink))?;
                run.passive_stop = Some(stop);
                run.passive_handle = Some(handle);
                run.mode = CaptureMode::PassiveEdge;
            }
            CaptureMode::ActiveForward => {
                let (thread_id, handle) = start_active_hooks(Arc::clone(sink))?;
                run.hook_thread_id = Some(thread_id);
                run.hook_handle = Some(handle);
                run.can_suppress = true;
                run.mode = CaptureMode::ActiveForward;
            }
        }
        Ok(())
    }

    fn stop(&self) -> PlatformResult<()> {
        let mut run = lock_recover(&self.inner);
        teardown(&mut run);
        Ok(())
    }

    fn can_suppress(&self) -> bool {
        lock_recover(&self.inner).can_suppress
    }

    fn current_mode(&self) -> CaptureMode {
        lock_recover(&self.inner).mode
    }
}

fn passive_poll_thread(sink: Arc<dyn InputSink>, stop: Arc<AtomicBool>) {
    let displays = active_display_bounds().unwrap_or_default();
    let mut last: Option<(i32, i32)> = None;
    while !stop.load(Ordering::Acquire) {
        if let Ok(point) = inject::cursor_position() {
            let pos = (point.x, point.y);
            if last != Some(pos) {
                let (dx, dy) = match last {
                    Some((px, py)) => (point.x - px, point.y - py),
                    None => (0, 0),
                };
                last = Some(pos);
                let event = virtual_point_to_event(&displays, point.x, point.y, dx, dy);
                let _ = catch_unwind(AssertUnwindSafe(|| sink.on_event(event)));
            }
        }
        std::thread::sleep(PASSIVE_POLL_INTERVAL);
    }
}

fn run_capture_thread(tx: std::sync::mpsc::Sender<Result<u32, String>>) {
    let thread_id = unsafe { GetCurrentThreadId() };
    create_message_queue();

    let worker_stop = Arc::new(AtomicBool::new(false));
    let worker_stop_for_thread = Arc::clone(&worker_stop);
    let worker = match std::thread::Builder::new()
        .name("mouser-win-capture-worker".into())
        .spawn(move || capture_worker(worker_stop_for_thread))
    {
        Ok(worker) => worker,
        Err(e) => {
            clear_capture_state();
            let _ = tx.send(Err(format!("capture worker start failed: {e}")));
            return;
        }
    };

    let keyboard_hook =
        match unsafe { SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard_hook), None, 0) } {
            Ok(hook) => hook,
            Err(e) => {
                stop_capture_worker(worker_stop, worker);
                clear_capture_state();
                let _ = tx.send(Err(format!("WH_KEYBOARD_LL install failed: {e}")));
                return;
            }
        };

    let mouse_hook = match unsafe { SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_hook), None, 0) } {
        Ok(hook) => hook,
        Err(e) => {
            let _ = unsafe { UnhookWindowsHookEx(keyboard_hook) };
            stop_capture_worker(worker_stop, worker);
            clear_capture_state();
            let _ = tx.send(Err(format!("WH_MOUSE_LL install failed: {e}")));
            return;
        }
    };

    // Switch the motion source to Raw Input (WM_INPUT) relative deltas so motion keeps
    // flowing while the OS cursor is pinned at the screen edge by hook suppression. On any
    // failure we leave raw inactive and the hook's absolute-delta path handles motion (no
    // regression). `raw_sink` is kept for teardown.
    let raw_sink = setup_raw_input();

    let _ = tx.send(Ok(thread_id));
    // Run the message pump under catch_unwind so a panic can't skip the teardown below
    // (which would leak the installed hooks / Raw Input registration on this live process).
    let _ = catch_unwind(AssertUnwindSafe(message_loop));

    teardown_raw_input(raw_sink);
    let _ = unsafe { UnhookWindowsHookEx(mouse_hook) };
    let _ = unsafe { UnhookWindowsHookEx(keyboard_hook) };
    stop_capture_worker(worker_stop, worker);
    clear_capture_state();
}

/// Register a Raw Input mouse sink so motion comes from device-level relative deltas
/// (`WM_INPUT`). Those keep flowing regardless of where the OS clamps the cursor, so NO
/// cursor pinning (`ClipCursor`) is needed — and we deliberately avoid it: `ClipCursor` is
/// a system-wide setting that survives this process, so a crash/kill mid-cross would leave
/// the user's cursor clamped ("unusable mouse"). Returns the sink window on success (raw
/// active); `None` leaves the absolute-delta fallback.
fn setup_raw_input() -> Option<HWND> {
    let Some(hwnd) = capture_rawinput::create_sink_window() else {
        eprintln!("mouser: raw-input sink window creation failed; using absolute-delta fallback");
        return None;
    };
    if !capture_rawinput::register_raw_mouse(hwnd) {
        eprintln!("mouser: raw-input mouse registration failed; using absolute-delta fallback");
        capture_rawinput::destroy_sink_window(hwnd);
        return None;
    }
    set_raw_input_active(true);
    Some(hwnd)
}

/// Tear down the Raw Input registration / sink window (both per-process, so they're also
/// reclaimed on process exit). No cursor clip to release — see [`setup_raw_input`].
fn teardown_raw_input(raw_sink: Option<HWND>) {
    let Some(hwnd) = raw_sink else {
        return;
    };
    capture_rawinput::unregister_raw_mouse();
    capture_rawinput::destroy_sink_window(hwnd);
    set_raw_input_active(false);
}

fn create_message_queue() {
    let mut msg = MSG::default();
    let _ = unsafe { PeekMessageW(&mut msg, None, 0, 0, PM_NOREMOVE) };
}

fn message_loop() {
    let mut msg = MSG::default();
    loop {
        let result = unsafe { GetMessageW(&mut msg, None, 0, 0) };
        if result.0 <= 0 {
            break;
        }
        // Dispatch so WM_INPUT reaches the Raw Input sink window's wndproc on this thread.
        let _ = unsafe { DispatchMessageW(&msg) };
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaptureAlreadyRunning;

impl std::fmt::Display for CaptureAlreadyRunning {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("a Windows input capture hook is already running")
    }
}

impl std::error::Error for CaptureAlreadyRunning {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaptureStartFailed(pub String);

impl std::fmt::Display for CaptureStartFailed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Windows low-level input capture could not start: {}",
            self.0
        )
    }
}

impl std::error::Error for CaptureStartFailed {}

#[cfg(test)]
#[path = "capture_tests.rs"]
mod tests;
