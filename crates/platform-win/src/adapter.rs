//! Windows adapter implementing `mouser_core` platform traits.
//!
//! [`WinInjector`] is the engine-facing wrapper over [`crate::inject`]. It maps
//! the platform-neutral trait contract (display-local cursor coordinates, wire
//! button ids, modifier bitmasks, and core scroll units) to the Win32 `SendInput`
//! backend.
//!
//! [`WinCapture`] is a **mode state machine** (`mouser_core::CaptureMode`) — the
//! Windows embodiment of "edge sensing is not input forwarding":
//!
//! - [`CaptureMode::Off`](mouser_core::platform::CaptureMode::Off) installs nothing.
//! - [`CaptureMode::PassiveEdge`](mouser_core::platform::CaptureMode::PassiveEdge)
//!   runs a lightweight background thread that polls `GetCursorPos` (via
//!   [`crate::inject::cursor_position`]) and reports cursor position to the sink. It
//!   installs **no** `WH_*_LL` hooks, never suppresses, and never observes the
//!   keyboard — so a connected-but-idle controller leaves local keyboard/touchpad
//!   completely native (this is what fixes the Bluetooth-input degradation).
//! - [`CaptureMode::ActiveForward`](mouser_core::platform::CaptureMode::ActiveForward)
//!   installs the low-level `WH_KEYBOARD_LL` + `WH_MOUSE_LL` hooks on a background
//!   thread **only while this machine is actively driving a remote peer**. In this
//!   mode the hooks synchronously swallow local input (the machine is forwarding to
//!   the peer) and enqueue each event for a worker thread to hand to the sink for
//!   forwarding. The decision is statically "suppress while forwarding", so there is
//!   no lagging-atomic race between the hook callback and the worker.
//!
//! The engine runtime drives the mode from ownership state and only ever escalates
//! to `ActiveForward` for the window during which a peer owns this machine's input.

use std::collections::VecDeque;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, AtomicU16, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock, PoisonError};
use std::thread::JoinHandle;
use std::time::Duration;

use mouser_core::platform::{
    CaptureMode, InputCapture, InputInjection, InputSink, LocalInputEvent, PlatformError,
    PlatformResult, ScrollUnit as CoreScrollUnit,
};
use windows::Win32::Foundation::{LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFO,
};
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, GetMessageW, PeekMessageW, PostThreadMessageW, SetWindowsHookExW,
    UnhookWindowsHookEx, KBDLLHOOKSTRUCT, LLKHF_EXTENDED, LLKHF_INJECTED, LLMHF_INJECTED, MSG,
    MSLLHOOKSTRUCT, PM_NOREMOVE, WH_KEYBOARD_LL, WH_MOUSE_LL, WM_KEYDOWN, WM_KEYUP, WM_LBUTTONDOWN,
    WM_LBUTTONUP, WM_MBUTTONDOWN, WM_MBUTTONUP, WM_MOUSEHWHEEL, WM_MOUSEMOVE, WM_MOUSEWHEEL,
    WM_QUIT, WM_RBUTTONDOWN, WM_RBUTTONUP, WM_SYSKEYDOWN, WM_SYSKEYUP, WM_XBUTTONDOWN,
    WM_XBUTTONUP, XBUTTON1, XBUTTON2,
};

use crate::inject::{self, Button, ScrollUnit as WinScrollUnit};
use crate::keymap::scancode_to_hid_usage;

/// Windows input injector backed by `SendInput`.
#[derive(Debug, Default, Clone, Copy)]
pub struct WinInjector;

impl WinInjector {
    /// Create a stateless `SendInput` injector.
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

/// Bounds of one active monitor in Windows' virtual-desktop coordinate space.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DisplayBounds {
    /// Zero-based id from the current stable enumeration order.
    pub id: u32,
    /// Left edge in virtual-desktop pixels.
    pub left: i32,
    /// Top edge in virtual-desktop pixels.
    pub top: i32,
    /// Width in pixels.
    pub width: i32,
    /// Height in pixels.
    pub height: i32,
}

impl DisplayBounds {
    fn from_rect(id: u32, rect: RECT) -> Option<Self> {
        let width = rect.right - rect.left;
        let height = rect.bottom - rect.top;
        if width <= 0 || height <= 0 {
            return None;
        }
        Some(Self {
            id,
            left: rect.left,
            top: rect.top,
            width,
            height,
        })
    }

    fn local_to_virtual(self, x: i32, y: i32) -> (i32, i32) {
        let x = x.clamp(0, self.width - 1);
        let y = y.clamp(0, self.height - 1);
        (self.left + x, self.top + y)
    }

    fn contains_virtual(self, x: i32, y: i32) -> bool {
        x >= self.left && x < self.left + self.width && y >= self.top && y < self.top + self.height
    }

    fn virtual_to_local(self, x: i32, y: i32) -> (i32, i32) {
        (x - self.left, y - self.top)
    }
}

/// Enumerate active monitors in deterministic top-left order.
///
/// Windows `HMONITOR` values are handles, not stable wire ids. Until the engine
/// persists monitor identity, the adapter exposes the same pragmatic contract as
/// the desktop UI: current monitors sorted by `(top,left,right,bottom)` and
/// addressed by zero-based index.
pub fn active_display_bounds() -> PlatformResult<Vec<DisplayBounds>> {
    let mut rects: Vec<RECT> = Vec::new();
    // SAFETY: `rects` lives for the duration of the synchronous enumeration. The
    // callback casts `LPARAM` back to that same `Vec<RECT>` and only pushes
    // copied monitor rectangles.
    let ok = unsafe {
        EnumDisplayMonitors(
            None,
            None,
            Some(enum_monitor),
            LPARAM((&mut rects as *mut Vec<RECT>) as isize),
        )
    };
    if !ok.as_bool() {
        return Err(Box::new(windows::core::Error::from_thread()));
    }

    rects.sort_by_key(|r| (r.top, r.left, r.right, r.bottom));
    let mut displays = Vec::with_capacity(rects.len());
    for rect in rects {
        let id = displays.len() as u32;
        if let Some(bounds) = DisplayBounds::from_rect(id, rect) {
            displays.push(bounds);
        }
    }
    Ok(displays)
}

/// Return the current bounds for a display id.
pub fn display_bounds(display_id: u32) -> PlatformResult<DisplayBounds> {
    active_display_bounds()?
        .into_iter()
        .find(|b| b.id == display_id)
        .ok_or_else(|| boxed(UnknownDisplay(display_id)))
}

unsafe extern "system" fn enum_monitor(
    monitor: HMONITOR,
    _hdc: HDC,
    _rect: *mut RECT,
    data: LPARAM,
) -> windows::core::BOOL {
    let rects = unsafe { &mut *(data.0 as *mut Vec<RECT>) };
    let mut info = MONITORINFO {
        cbSize: std::mem::size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    // SAFETY: `info` is a valid writable `MONITORINFO` with `cbSize` set.
    if unsafe { GetMonitorInfoW(monitor, &mut info) }.as_bool() {
        rects.push(info.rcMonitor);
    }
    true.into()
}

/// Error when a wire `display_id` matches no active monitor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnknownDisplay(pub u32);

impl std::fmt::Display for UnknownDisplay {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "no active Windows display with id {}", self.0)
    }
}

impl std::error::Error for UnknownDisplay {}

/// A bad button index (only 0..=4 are defined, §7.5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnknownButton(pub u8);

impl std::fmt::Display for UnknownButton {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "pointer button index {} is not defined (§7.5)", self.0)
    }
}

impl std::error::Error for UnknownButton {}

fn button_of(index: u8) -> Result<Button, UnknownButton> {
    Button::from_wire(index).ok_or(UnknownButton(index))
}

fn scroll_unit(unit: CoreScrollUnit) -> WinScrollUnit {
    match unit {
        CoreScrollUnit::Detent120 => WinScrollUnit::Detent120,
        CoreScrollUnit::LogicalPixel => WinScrollUnit::LogicalPixel,
    }
}

fn boxed<E: std::error::Error + Send + Sync + 'static>(e: E) -> PlatformError {
    Box::new(e)
}

fn modifier_usages(mods: u16) -> Vec<u16> {
    const MOD_BITS: [(u16, u16); 8] = [
        (0, 0xE0),
        (1, 0xE1),
        (2, 0xE2),
        (3, 0xE3),
        (4, 0xE4),
        (5, 0xE5),
        (6, 0xE6),
        (7, 0xE7),
    ];
    let mut out = Vec::new();
    for (bit, usage) in MOD_BITS {
        if mods & (1 << bit) != 0 {
            out.push(usage);
        }
    }
    out
}

impl InputInjection for WinInjector {
    fn move_cursor(&self, display_id: u32, x: i32, y: i32) -> PlatformResult<()> {
        let bounds = display_bounds(display_id)?;
        let (vx, vy) = bounds.local_to_virtual(x, y);
        inject::move_cursor(vx, vy).map_err(boxed)
    }

    fn move_cursor_relative(&self, dx: i32, dy: i32) -> PlatformResult<()> {
        inject::move_cursor_relative(dx, dy).map_err(boxed)
    }

    fn button(&self, button: u8, down: bool) -> PlatformResult<()> {
        let button = button_of(button).map_err(boxed)?;
        inject::button(button, down).map_err(boxed)
    }

    fn key(&self, usage: u16, down: bool, mods: u16) -> PlatformResult<()> {
        let modifiers = modifier_usages(mods);
        if down {
            for m in &modifiers {
                inject::key(*m, true).map_err(boxed)?;
            }
            inject::key(usage, true).map_err(boxed)?;
        } else {
            inject::key(usage, false).map_err(boxed)?;
            for m in modifiers.iter().rev() {
                inject::key(*m, false).map_err(boxed)?;
            }
        }
        Ok(())
    }

    fn scroll(&self, dx: i32, dy: i32, unit: CoreScrollUnit) -> PlatformResult<()> {
        inject::scroll(dx, dy, scroll_unit(unit)).map_err(boxed)
    }
}

/// How often [`CaptureMode::PassiveEdge`] samples the cursor. `GetCursorPos` is a
/// cheap kernel read (no hook, no per-input-event cost), so ~125 Hz gives prompt
/// edge detection without touching the local input pipeline at all.
const PASSIVE_POLL_INTERVAL: Duration = Duration::from_millis(8);

/// Shared mutable state used by the static Win32 hook callbacks (the
/// [`CaptureMode::ActiveForward`] path only).
#[derive(Default)]
struct CaptureState {
    sink: Option<Arc<dyn InputSink>>,
    displays: Vec<DisplayBounds>,
}

struct CaptureQueue {
    pending: Mutex<VecDeque<QueuedCaptureEvent>>,
    ready: Condvar,
}

#[derive(Clone, Copy)]
enum QueuedCaptureEvent {
    Event(LocalInputEvent),
    CursorPoint { x: i32, y: i32 },
}

const MAX_CAPTURE_QUEUE: usize = 256;

static CAPTURE_STATE: OnceLock<Mutex<CaptureState>> = OnceLock::new();
static CAPTURE_QUEUE: OnceLock<CaptureQueue> = OnceLock::new();
static CAPTURE_MODS: AtomicU16 = AtomicU16::new(0);

fn capture_state() -> &'static Mutex<CaptureState> {
    CAPTURE_STATE.get_or_init(|| Mutex::new(CaptureState::default()))
}

fn capture_queue() -> &'static CaptureQueue {
    CAPTURE_QUEUE.get_or_init(|| CaptureQueue {
        pending: Mutex::new(VecDeque::new()),
        ready: Condvar::new(),
    })
}

fn lock_recover<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

fn wait_recover<'a, T>(condvar: &Condvar, guard: MutexGuard<'a, T>) -> MutexGuard<'a, T> {
    condvar.wait(guard).unwrap_or_else(PoisonError::into_inner)
}

fn clear_capture_state() {
    CAPTURE_MODS.store(0, Ordering::Release);
    clear_capture_queue();
    let mut state = lock_recover(capture_state());
    state.sink = None;
    state.displays.clear();
}

fn prepare_capture_state(sink: Arc<dyn InputSink>) -> PlatformResult<()> {
    let mut state = lock_recover(capture_state());
    if state.sink.is_some() {
        return Err(boxed(CaptureAlreadyRunning));
    }
    CAPTURE_MODS.store(0, Ordering::Release);
    clear_capture_queue();
    state.displays = active_display_bounds().unwrap_or_default();
    state.sink = Some(sink);
    Ok(())
}

/// Windows input capture: a [`CaptureMode`] state machine over a passive cursor
/// poll (PassiveEdge) and the low-level `WH_*_LL` hooks (ActiveForward).
pub struct WinCapture {
    inner: Arc<Mutex<CaptureRun>>,
}

/// The adapter's run state. All transitions go through the `inner` mutex, and the
/// background threads it owns (the passive poll thread, the hook thread, and the
/// hook thread's worker) **never** acquire this mutex — so a transition can join
/// them while holding the lock without risking a deadlock.
struct CaptureRun {
    /// The mode the adapter is actually in.
    mode: CaptureMode,
    /// Active-forward hook thread id (for `PostThreadMessageW(WM_QUIT)`).
    hook_thread_id: Option<u32>,
    /// Active-forward hook thread handle.
    hook_handle: Option<JoinHandle<()>>,
    /// True only while the `WH_*_LL` hooks are installed.
    can_suppress: bool,
    /// Passive poll thread stop flag.
    passive_stop: Option<Arc<AtomicBool>>,
    /// Passive poll thread handle.
    passive_handle: Option<JoinHandle<()>>,
}

impl Default for WinCapture {
    fn default() -> Self {
        Self::new()
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

/// Tear down whatever the run is currently doing and leave it in
/// [`CaptureMode::Off`]. Joins the background threads; they never touch `run`'s
/// mutex, so it is safe to hold the guard across the joins.
fn teardown(run: &mut CaptureRun) {
    // Stop the passive poll thread.
    if let Some(stop) = run.passive_stop.take() {
        stop.store(true, Ordering::Release);
    }
    if let Some(handle) = run.passive_handle.take() {
        let _ = handle.join();
    }

    // Stop the active-forward hook thread: ask its message loop to unwind + unhook.
    let hook_tid = run.hook_thread_id.take();
    if let Some(tid) = hook_tid {
        // SAFETY: `tid` was reported by the hook thread after it created its User32
        // message queue; WM_QUIT asks its GetMessageW loop to return and unhook.
        let _ = unsafe { PostThreadMessageW(tid, WM_QUIT, WPARAM(0), LPARAM(0)) };
    }
    if let Some(handle) = run.hook_handle.take() {
        // Never self-join. Teardown runs on the runtime's capture-mode task, not the
        // hook thread, but guard defensively anyway.
        // SAFETY: thread-id query has no preconditions.
        let safe = hook_tid.is_none_or(|tid| unsafe { GetCurrentThreadId() } != tid);
        if safe {
            let _ = handle.join();
        }
    }

    run.can_suppress = false;
    run.mode = CaptureMode::Off;
}

/// Spawn the passive cursor-poll thread. Returns its stop flag and join handle.
fn start_passive_poll(sink: Arc<dyn InputSink>) -> PlatformResult<(Arc<AtomicBool>, JoinHandle<()>)> {
    let stop = Arc::new(AtomicBool::new(false));
    let stop_for_thread = Arc::clone(&stop);
    let handle = std::thread::Builder::new()
        .name("mouser-win-edge".into())
        .spawn(move || passive_poll_thread(sink, stop_for_thread))
        .map_err(boxed)?;
    Ok((stop, handle))
}

/// Bring up the active-forward hooks. Returns the hook thread id (for WM_QUIT) and
/// its join handle once both hooks are installed, or an error if either failed.
fn start_active_hooks(sink: Arc<dyn InputSink>) -> PlatformResult<(u32, JoinHandle<()>)> {
    prepare_capture_state(sink)?;
    let (tx, rx) = std::sync::mpsc::channel::<Result<u32, String>>();
    let handle = std::thread::Builder::new()
        .name("mouser-win-capture".into())
        .spawn(move || run_capture_thread(tx))
        .map_err(boxed)?;

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
            return Ok(()); // idempotent: already in the requested mode
        }
        // Tear the previous mode down first (leaves us in Off), then bring up the new
        // one. On failure we stay in Off rather than a half-installed state.
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

/// The passive edge sensor: poll `GetCursorPos`, map to a display-local position,
/// and report it to the sink. Installs no hooks, never suppresses, never reads the
/// keyboard — so local input is wholly untouched while a controller is connected
/// but not actively driving the peer.
fn passive_poll_thread(sink: Arc<dyn InputSink>, stop: Arc<AtomicBool>) {
    let displays = active_display_bounds().unwrap_or_default();
    let mut last: Option<(i32, i32)> = None;
    while !stop.load(Ordering::Acquire) {
        if let Ok(point) = inject::cursor_position() {
            let pos = (point.x, point.y);
            if last != Some(pos) {
                last = Some(pos);
                let event = virtual_point_to_event(&displays, point.x, point.y);
                // Passive sensing never suppresses; the returned decision is ignored.
                let _ = catch_unwind(AssertUnwindSafe(|| sink.on_event(event)));
            }
        }
        std::thread::sleep(PASSIVE_POLL_INTERVAL);
    }
}

/// The active-forward hook thread: install `WH_KEYBOARD_LL` + `WH_MOUSE_LL`, run the
/// message loop, and unhook on WM_QUIT. Reports its thread id back over `tx` once the
/// hooks are installed (so a transition can post WM_QUIT to it), or an error string
/// if installation failed. Never touches the [`CaptureRun`] mutex.
fn run_capture_thread(tx: std::sync::mpsc::Sender<Result<u32, String>>) {
    // SAFETY: thread id is a pure OS query.
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

    // SAFETY: low-level hook procedures are static `extern "system"` functions with
    // the exact signature Windows requires. For WH_*_LL hooks the callback runs in
    // this installing thread's context while its message loop is alive.
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

    // SAFETY: see keyboard hook install above.
    let mouse_hook = match unsafe { SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_hook), None, 0) } {
        Ok(hook) => hook,
        Err(e) => {
            // SAFETY: `keyboard_hook` was returned by SetWindowsHookExW above and is
            // still owned by this thread.
            let _ = unsafe { UnhookWindowsHookEx(keyboard_hook) };
            stop_capture_worker(worker_stop, worker);
            clear_capture_state();
            let _ = tx.send(Err(format!("WH_MOUSE_LL install failed: {e}")));
            return;
        }
    };

    let _ = tx.send(Ok(thread_id));

    message_loop();

    // SAFETY: these handles were installed on this thread and have not been unhooked
    // yet. Ignore shutdown failures; capture is already stopping.
    let _ = unsafe { UnhookWindowsHookEx(mouse_hook) };
    let _ = unsafe { UnhookWindowsHookEx(keyboard_hook) };
    stop_capture_worker(worker_stop, worker);
    clear_capture_state();
}

fn stop_capture_worker(stop: Arc<AtomicBool>, worker: JoinHandle<()>) {
    stop.store(true, Ordering::Release);
    capture_queue().ready.notify_all();
    let _ = worker.join();
}

fn capture_worker(stop: Arc<AtomicBool>) {
    loop {
        let events = wait_for_capture_events(&stop);
        if events.is_empty() {
            if stop.load(Ordering::Acquire) {
                break;
            }
            continue;
        }

        for event in events {
            process_queued_capture_event(event);
        }
    }
}

fn wait_for_capture_events(stop: &AtomicBool) -> VecDeque<QueuedCaptureEvent> {
    let queue = capture_queue();
    let mut pending = lock_recover(&queue.pending);
    while pending.is_empty() && !stop.load(Ordering::Acquire) {
        pending = wait_recover(&queue.ready, pending);
    }

    let mut events = VecDeque::new();
    std::mem::swap(&mut *pending, &mut events);
    events
}

fn create_message_queue() {
    let mut msg = MSG::default();
    // SAFETY: a zero-filter PeekMessageW creates this thread's User32 message queue
    // if it does not already exist. PM_NOREMOVE leaves queued messages untouched,
    // which matters because a transition later uses PostThreadMessageW.
    let _ = unsafe { PeekMessageW(&mut msg, None, 0, 0, PM_NOREMOVE) };
}

fn message_loop() {
    let mut msg = MSG::default();
    loop {
        // SAFETY: `msg` is a valid writable MSG and no HWND filter is requested.
        let result = unsafe { GetMessageW(&mut msg, None, 0, 0) };
        if result.0 <= 0 {
            break;
        }
    }
}

unsafe extern "system" fn keyboard_hook(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code < 0 {
        return call_next(code, wparam, lparam);
    }
    let Some(message) = hook_message(wparam) else {
        return call_next(code, wparam, lparam);
    };
    if lparam.0 == 0 {
        return call_next(code, wparam, lparam);
    }

    // SAFETY: Windows calls this hook with `lparam` pointing at a live
    // KBDLLHOOKSTRUCT for the duration of the callback.
    let hook = unsafe { *(lparam.0 as *const KBDLLHOOKSTRUCT) };
    // Never capture our own injected input.
    if hook.flags.0 & LLKHF_INJECTED.0 != 0 {
        return call_next(code, wparam, lparam);
    }
    // Only real key transitions are forwarded/suppressed; anything else passes.
    if !is_key_message(message) {
        return call_next(code, wparam, lparam);
    }

    // Hooks only exist in ActiveForward (we are driving the peer), so the answer is
    // statically "swallow locally and forward": enqueue the modeled event for the
    // worker and return Suppress synchronously. Unmapped keys are still swallowed so
    // they cannot leak to the local desktop while controlling.
    //
    // Known minor edge case: a key physically held *across* the PassiveEdge→
    // ActiveForward transition had its key-down delivered locally (passive mode does
    // not capture the keyboard); its key-up then arrives here and is suppressed,
    // which can leave that one key logically held on the local desktop until pressed
    // again. This requires holding a key while crossing the screen edge and is far
    // rarer than the always-on-hook degradation this design removes; synthesizing
    // held-key releases at the transition is future work.
    if let Some(event) = keyboard_event_from_parts(message, hook.scanCode, hook.flags.0) {
        enqueue_capture_event(event);
    }
    LRESULT(1)
}

unsafe extern "system" fn mouse_hook(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code < 0 {
        return call_next(code, wparam, lparam);
    }
    let Some(message) = hook_message(wparam) else {
        return call_next(code, wparam, lparam);
    };
    if lparam.0 == 0 {
        return call_next(code, wparam, lparam);
    }

    // SAFETY: Windows calls this hook with `lparam` pointing at a live
    // MSLLHOOKSTRUCT for the duration of the callback.
    let hook = unsafe { *(lparam.0 as *const MSLLHOOKSTRUCT) };
    if hook.flags & LLMHF_INJECTED != 0 {
        return call_next(code, wparam, lparam);
    }

    // ActiveForward: swallow local pointer input and forward it to the peer.
    if message == WM_MOUSEMOVE {
        enqueue_cursor_capture_point(hook.pt.x, hook.pt.y);
        return LRESULT(1);
    }
    if let Some(event) =
        mouse_event_from_parts(message, hook.pt.x, hook.pt.y, hook.mouseData, hook.flags)
    {
        enqueue_capture_event(event);
        return LRESULT(1);
    }
    call_next(code, wparam, lparam)
}

fn call_next(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    // SAFETY: forwarding with a null hook handle is the documented low-level hook
    // pattern; `code/wparam/lparam` are the values Windows supplied.
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

fn hook_message(wparam: WPARAM) -> Option<u32> {
    u32::try_from(wparam.0).ok()
}

fn is_key_message(message: u32) -> bool {
    matches!(
        message,
        WM_KEYDOWN | WM_KEYUP | WM_SYSKEYDOWN | WM_SYSKEYUP
    )
}

fn keyboard_event_from_parts(message: u32, scan_code: u32, flags: u32) -> Option<LocalInputEvent> {
    let down = match message {
        WM_KEYDOWN | WM_SYSKEYDOWN => true,
        WM_KEYUP | WM_SYSKEYUP => false,
        _ => return None,
    };
    if flags & LLKHF_INJECTED.0 != 0 {
        return None;
    }

    let code = u16::try_from(scan_code).ok()?;
    let extended = flags & LLKHF_EXTENDED.0 != 0;
    let usage = scancode_to_hid_usage(code, extended)?;
    let mods = update_modifier_state(usage, down);
    Some(LocalInputEvent::Key { usage, down, mods })
}

fn update_modifier_state(usage: u16, down: bool) -> u16 {
    let Some(bit) = modifier_bit(usage) else {
        return CAPTURE_MODS.load(Ordering::Acquire);
    };
    if down {
        CAPTURE_MODS.fetch_or(bit, Ordering::AcqRel) | bit
    } else {
        CAPTURE_MODS.fetch_and(!bit, Ordering::AcqRel) & !bit
    }
}

fn modifier_bit(usage: u16) -> Option<u16> {
    if (0xE0..=0xE7).contains(&usage) {
        Some(1 << (usage - 0xE0))
    } else {
        None
    }
}

fn mouse_event_from_parts(
    message: u32,
    x: i32,
    y: i32,
    mouse_data: u32,
    flags: u32,
) -> Option<LocalInputEvent> {
    if flags & LLMHF_INJECTED != 0 {
        return None;
    }

    match message {
        WM_MOUSEMOVE => Some(cursor_event_for_virtual_point(x, y)),
        WM_LBUTTONDOWN => Some(LocalInputEvent::Button {
            button: 0,
            down: true,
        }),
        WM_LBUTTONUP => Some(LocalInputEvent::Button {
            button: 0,
            down: false,
        }),
        WM_RBUTTONDOWN => Some(LocalInputEvent::Button {
            button: 1,
            down: true,
        }),
        WM_RBUTTONUP => Some(LocalInputEvent::Button {
            button: 1,
            down: false,
        }),
        WM_MBUTTONDOWN => Some(LocalInputEvent::Button {
            button: 2,
            down: true,
        }),
        WM_MBUTTONUP => Some(LocalInputEvent::Button {
            button: 2,
            down: false,
        }),
        WM_XBUTTONDOWN | WM_XBUTTONUP => {
            x_button(mouse_data).map(|button| LocalInputEvent::Button {
                button,
                down: message == WM_XBUTTONDOWN,
            })
        }
        WM_MOUSEWHEEL => Some(LocalInputEvent::Scroll {
            dx: 0,
            dy: i32::from(high_word_i16(mouse_data)),
        }),
        WM_MOUSEHWHEEL => Some(LocalInputEvent::Scroll {
            dx: i32::from(high_word_i16(mouse_data)),
            dy: 0,
        }),
        _ => None,
    }
}

/// Map a virtual-desktop point to a display-local [`LocalInputEvent::CursorMoved`].
fn virtual_point_to_event(displays: &[DisplayBounds], x: i32, y: i32) -> LocalInputEvent {
    let bounds = displays
        .iter()
        .copied()
        .find(|b| b.contains_virtual(x, y))
        .or_else(|| displays.first().copied());

    match bounds {
        Some(bounds) => {
            let (lx, ly) = bounds.virtual_to_local(x, y);
            LocalInputEvent::CursorMoved {
                display_id: bounds.id,
                x: lx,
                y: ly,
            }
        }
        None => LocalInputEvent::CursorMoved {
            display_id: 0,
            x,
            y,
        },
    }
}

/// Resolve a virtual-desktop point using the active-forward display snapshot.
fn cursor_event_for_virtual_point(x: i32, y: i32) -> LocalInputEvent {
    let displays = lock_recover(capture_state()).displays.clone();
    virtual_point_to_event(&displays, x, y)
}

/// Hand one queued event to the sink. The returned decision is unused: in
/// ActiveForward the hook already returned Suppress synchronously, so this call
/// exists only to drive forwarding (and a reclaim arrives as a `SetCaptureMode`
/// action that tears the hooks back down).
fn dispatch_capture_event(event: LocalInputEvent) {
    let sink = lock_recover(capture_state()).sink.clone();
    if let Some(sink) = sink {
        let _ = catch_unwind(AssertUnwindSafe(|| sink.on_event(event)));
    }
}

fn process_queued_capture_event(event: QueuedCaptureEvent) {
    match event {
        QueuedCaptureEvent::Event(event) => dispatch_capture_event(event),
        QueuedCaptureEvent::CursorPoint { x, y } => {
            dispatch_capture_event(cursor_event_for_virtual_point(x, y))
        }
    }
}

fn enqueue_capture_event(event: LocalInputEvent) {
    enqueue_queued_capture_event(QueuedCaptureEvent::Event(event));
}

fn enqueue_cursor_capture_point(x: i32, y: i32) {
    enqueue_queued_capture_event(QueuedCaptureEvent::CursorPoint { x, y });
}

fn enqueue_queued_capture_event(event: QueuedCaptureEvent) {
    let queue = capture_queue();
    // Block rather than `try_lock`: the worker only ever holds this lock for the
    // microsecond-scale `swap` in `wait_for_capture_events` (it releases it across
    // the condvar wait and while running the sink), so this never stalls the hook
    // long enough to trip Windows' LowLevelHooksTimeout — and it guarantees a
    // key/button transition is never dropped just because the worker was mid-swap.
    let mut pending = lock_recover(&queue.pending);

    // Coalesce consecutive cursor moves: only the latest position matters. A cursor
    // event is only ever merged into another cursor event, so this can never drop a
    // key/button transition (a transition between two cursors blocks the merge).
    if queued_capture_event_is_cursor(&event) {
        if let Some(last) = pending.back_mut() {
            if queued_capture_event_is_cursor(last) {
                *last = event;
                queue.ready.notify_one();
                return;
            }
        }
    }

    if pending.len() >= MAX_CAPTURE_QUEUE {
        // Overflow under a slow sink: evict the oldest *cursor* (positional, lossy)
        // event so key/button transitions survive. Only if the backlog is entirely
        // transitions do we fall back to dropping the oldest entry.
        drop_one_for_overflow(&mut pending);
    }
    pending.push_back(event);
    queue.ready.notify_one();
}

/// Evict one event to make room: prefer the oldest cursor move (lossy), and only
/// drop the front (oldest transition) if there is no cursor to sacrifice.
fn drop_one_for_overflow(pending: &mut VecDeque<QueuedCaptureEvent>) {
    if let Some(idx) = pending.iter().position(queued_capture_event_is_cursor) {
        let _ = pending.remove(idx);
    } else {
        let _ = pending.pop_front();
    }
}

fn queued_capture_event_is_cursor(event: &QueuedCaptureEvent) -> bool {
    matches!(
        event,
        QueuedCaptureEvent::CursorPoint { .. }
            | QueuedCaptureEvent::Event(LocalInputEvent::CursorMoved { .. })
    )
}

fn clear_capture_queue() {
    lock_recover(&capture_queue().pending).clear();
    capture_queue().ready.notify_all();
}

fn high_word_u16(value: u32) -> u16 {
    ((value >> 16) & 0xFFFF) as u16
}

fn high_word_i16(value: u32) -> i16 {
    high_word_u16(value) as i16
}

fn x_button(mouse_data: u32) -> Option<u8> {
    match high_word_u16(mouse_data) {
        x if x == XBUTTON1 => Some(3),
        x if x == XBUTTON2 => Some(4),
        _ => None,
    }
}

/// Another Windows capture hook is already active in this process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaptureAlreadyRunning;

impl std::fmt::Display for CaptureAlreadyRunning {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("a Windows input capture hook is already running")
    }
}

impl std::error::Error for CaptureAlreadyRunning {}

/// The low-level Windows hook thread could not be started.
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
mod tests {
    use super::*;

    use mouser_core::platform::CaptureDecision;
    use std::collections::VecDeque;

    static CAPTURE_TEST_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

    fn capture_test_lock() -> MutexGuard<'static, ()> {
        lock_recover(CAPTURE_TEST_LOCK.get_or_init(|| Mutex::new(())))
    }

    /// Records every event handed to it; the decision is unused under the new model
    /// (the active hook suppresses synchronously), so it always passes through.
    #[derive(Default)]
    struct RecordingSink {
        events: Mutex<Vec<LocalInputEvent>>,
    }

    impl RecordingSink {
        fn events(&self) -> Vec<LocalInputEvent> {
            lock_recover(&self.events).clone()
        }
    }

    impl InputSink for RecordingSink {
        fn on_event(&self, event: LocalInputEvent) -> CaptureDecision {
            lock_recover(&self.events).push(event);
            CaptureDecision::PassThrough
        }
    }

    fn install_test_sink(sink: &Arc<RecordingSink>) {
        let sink_trait: Arc<dyn InputSink> = sink.clone();
        lock_recover(capture_state()).sink = Some(sink_trait);
    }

    fn drain_capture_queue_for_test() {
        let events = {
            let mut pending = lock_recover(&capture_queue().pending);
            std::mem::take(&mut *pending)
        };
        for event in events {
            process_queued_capture_event(event);
        }
    }

    fn key(usage: u16, down: bool) -> LocalInputEvent {
        LocalInputEvent::Key {
            usage,
            down,
            mods: 0,
        }
    }

    fn cursor(x: i32, y: i32) -> LocalInputEvent {
        LocalInputEvent::CursorMoved {
            display_id: 0,
            x,
            y,
        }
    }

    #[test]
    fn display_bounds_clamp_local_coords() {
        let bounds = DisplayBounds {
            id: 2,
            left: -1920,
            top: 100,
            width: 1920,
            height: 1080,
        };
        assert_eq!(bounds.local_to_virtual(20, 30), (-1900, 130));
        assert_eq!(bounds.local_to_virtual(-5, -8), (-1920, 100));
        assert_eq!(bounds.local_to_virtual(4000, 4000), (-1, 1179));
    }

    #[test]
    fn button_indices_match_wire_catalog() {
        assert_eq!(button_of(0), Ok(Button::Left));
        assert_eq!(button_of(4), Ok(Button::Forward));
        assert_eq!(button_of(5), Err(UnknownButton(5)));
    }

    #[test]
    fn modifier_bits_map_to_hid_usages() {
        assert_eq!(
            modifier_usages((1 << 0) | (1 << 3) | (1 << 7)),
            vec![0xE0, 0xE3, 0xE7]
        );
        assert!(modifier_usages(1 << 12).is_empty());
    }

    #[test]
    fn captured_keyboard_events_use_hid_usages() {
        let _guard = capture_test_lock();
        clear_capture_state();
        assert_eq!(
            keyboard_event_from_parts(WM_KEYDOWN, 0x1E, 0),
            Some(LocalInputEvent::Key {
                usage: 0x04,
                down: true,
                mods: 0,
            })
        );
        assert_eq!(
            keyboard_event_from_parts(WM_KEYUP, 0x1E, 0),
            Some(LocalInputEvent::Key {
                usage: 0x04,
                down: false,
                mods: 0,
            })
        );
        assert_eq!(
            keyboard_event_from_parts(WM_KEYDOWN, 0x1E, LLKHF_INJECTED.0),
            None
        );
        clear_capture_state();
    }

    #[test]
    fn captured_keyboard_tracks_modifier_state() {
        let _guard = capture_test_lock();
        clear_capture_state();
        assert_eq!(
            keyboard_event_from_parts(WM_KEYDOWN, 0x1D, 0),
            Some(LocalInputEvent::Key {
                usage: 0xE0,
                down: true,
                mods: 1,
            })
        );
        assert_eq!(
            keyboard_event_from_parts(WM_KEYDOWN, 0x1E, 0),
            Some(LocalInputEvent::Key {
                usage: 0x04,
                down: true,
                mods: 1,
            })
        );
        assert_eq!(
            keyboard_event_from_parts(WM_KEYUP, 0x1D, 0),
            Some(LocalInputEvent::Key {
                usage: 0xE0,
                down: false,
                mods: 0,
            })
        );
        clear_capture_state();
    }

    #[test]
    fn captured_mouse_buttons_and_wheel_map_to_core_events() {
        assert_eq!(
            mouse_event_from_parts(WM_LBUTTONDOWN, 0, 0, 0, 0),
            Some(LocalInputEvent::Button {
                button: 0,
                down: true,
            })
        );
        assert_eq!(
            mouse_event_from_parts(WM_XBUTTONUP, 0, 0, u32::from(XBUTTON2) << 16, 0),
            Some(LocalInputEvent::Button {
                button: 4,
                down: false,
            })
        );

        let negative_wheel = u32::from((-120_i16) as u16) << 16;
        assert_eq!(
            mouse_event_from_parts(WM_MOUSEWHEEL, 0, 0, negative_wheel, 0),
            Some(LocalInputEvent::Scroll { dx: 0, dy: -120 })
        );
        assert_eq!(
            mouse_event_from_parts(WM_MOUSEWHEEL, 0, 0, negative_wheel, LLMHF_INJECTED),
            None
        );
    }

    #[test]
    fn captured_cursor_resolves_virtual_point_to_display_local_coords() {
        let _guard = capture_test_lock();
        clear_capture_state();
        lock_recover(capture_state()).displays = vec![DisplayBounds {
            id: 7,
            left: -100,
            top: 50,
            width: 200,
            height: 100,
        }];

        assert_eq!(
            cursor_event_for_virtual_point(-10, 80),
            LocalInputEvent::CursorMoved {
                display_id: 7,
                x: 90,
                y: 30,
            }
        );
        clear_capture_state();
    }

    #[test]
    fn new_capture_is_off_and_cannot_suppress() {
        let cap = WinCapture::new();
        assert_eq!(cap.current_mode(), CaptureMode::Off);
        assert!(!cap.can_suppress());
    }

    #[test]
    fn passive_mode_installs_no_hooks() {
        let _guard = capture_test_lock();
        let cap = WinCapture::new();
        let sink: Arc<dyn InputSink> = Arc::new(RecordingSink::default());

        cap.set_mode(CaptureMode::PassiveEdge, &sink)
            .expect("enter passive edge");
        assert_eq!(cap.current_mode(), CaptureMode::PassiveEdge);
        assert!(
            !cap.can_suppress(),
            "passive edge sensing never suppresses local input"
        );
        {
            let run = lock_recover(&cap.inner);
            assert!(
                run.hook_thread_id.is_none(),
                "no WH_*_LL hooks are installed in passive mode"
            );
            assert!(run.passive_handle.is_some(), "the poll thread is running");
        }

        // Re-entering the same mode is a no-op.
        cap.set_mode(CaptureMode::PassiveEdge, &sink)
            .expect("idempotent");
        assert_eq!(cap.current_mode(), CaptureMode::PassiveEdge);

        cap.stop().expect("stop");
        assert_eq!(cap.current_mode(), CaptureMode::Off);
        {
            let run = lock_recover(&cap.inner);
            assert!(run.passive_handle.is_none(), "poll thread joined on stop");
        }
    }

    #[test]
    fn coalescing_preserves_interleaved_key_transitions() {
        let _guard = capture_test_lock();
        clear_capture_state();
        let sink = Arc::new(RecordingSink::default());
        install_test_sink(&sink);

        // A key down/up straddling a run of cursor moves. The middle cursor (2,2)
        // coalesces into (3,3); both key transitions must survive in order.
        enqueue_cursor_capture_point(1, 1);
        enqueue_capture_event(key(0x04, true));
        enqueue_cursor_capture_point(2, 2);
        enqueue_cursor_capture_point(3, 3); // coalesces with (2,2)
        enqueue_capture_event(key(0x04, false));
        enqueue_cursor_capture_point(4, 4);
        drain_capture_queue_for_test();

        let events = sink.events();
        let keys: Vec<LocalInputEvent> = events
            .iter()
            .copied()
            .filter(|e| matches!(e, LocalInputEvent::Key { .. }))
            .collect();
        assert_eq!(
            keys,
            vec![key(0x04, true), key(0x04, false)],
            "both key transitions preserved, in order"
        );
        assert!(
            !events.contains(&cursor(2, 2)),
            "the intermediate cursor (2,2) coalesced away"
        );
        assert!(
            events.contains(&cursor(3, 3)) && events.contains(&cursor(4, 4)),
            "the latest cursor positions survive"
        );
        clear_capture_state();
    }

    #[test]
    fn high_rate_cursor_flood_never_overflows_or_evicts_transitions() {
        let _guard = capture_test_lock();
        clear_capture_state();
        let sink = Arc::new(RecordingSink::default());
        install_test_sink(&sink);

        // A held key, then a long flood of cursor moves (as a fast touchpad would
        // produce), then the release. Consecutive cursors coalesce into the tail, so
        // the queue never grows past a few entries and never overflows — the down/up
        // transitions are never at risk.
        enqueue_capture_event(key(0x04, true));
        for i in 0..(MAX_CAPTURE_QUEUE as i32 * 8) {
            enqueue_cursor_capture_point(i, i);
        }
        enqueue_capture_event(key(0x04, false));

        {
            let pending = lock_recover(&capture_queue().pending);
            assert!(
                pending.len() <= 3,
                "cursor flood coalesced; queue stayed tiny (len {})",
                pending.len()
            );
        }

        drain_capture_queue_for_test();
        let events = sink.events();
        assert!(events.contains(&key(0x04, true)), "key down preserved");
        assert!(events.contains(&key(0x04, false)), "key up preserved");
        clear_capture_state();
    }

    #[test]
    fn slow_sink_overflow_is_bounded_and_evicts_cursors_before_transitions() {
        let _guard = capture_test_lock();
        clear_capture_state();

        // Simulate a stalled worker with a realistic backlog where cursor moves
        // dominate: a leading key transition, then `pairs` of (cursor, button) so the
        // cursors don't coalesce. `pairs` is chosen so the queue overflows yet there
        // are always more cursors than overflow drops — the property that holds in
        // practice (positional events vastly outnumber transitions). The queue must
        // stay bounded and every key/button transition must survive (only cursors are
        // sacrificed).
        let pairs = MAX_CAPTURE_QUEUE * 3 / 4; // overflow, but drops < cursor count
        enqueue_capture_event(key(0x04, true));
        for i in 0..pairs as i32 {
            enqueue_cursor_capture_point(i, i);
            enqueue_capture_event(LocalInputEvent::Button {
                button: 2,
                down: i % 2 == 0,
            });
        }

        {
            let pending = lock_recover(&capture_queue().pending);
            assert!(
                pending.len() <= MAX_CAPTURE_QUEUE,
                "queue is bounded under a slow sink (len {})",
                pending.len()
            );
            assert!(
                pending
                    .iter()
                    .any(|e| matches!(e, QueuedCaptureEvent::Event(LocalInputEvent::Key { .. }))),
                "the leading key transition survived overflow (cursors evicted first)"
            );
            let transitions = pending
                .iter()
                .filter(|e| matches!(e, QueuedCaptureEvent::Event(LocalInputEvent::Button { .. })))
                .count();
            assert_eq!(
                transitions, pairs,
                "every button transition survived; only cursors were evicted"
            );
        }
        clear_capture_state();
    }

    #[test]
    fn overflow_eviction_prefers_cursors_then_falls_back_to_front() {
        // With a cursor present, the oldest cursor is evicted and transitions stay.
        let mut q: VecDeque<QueuedCaptureEvent> = VecDeque::new();
        q.push_back(QueuedCaptureEvent::Event(key(0x04, true)));
        q.push_back(QueuedCaptureEvent::CursorPoint { x: 1, y: 1 });
        q.push_back(QueuedCaptureEvent::Event(LocalInputEvent::Button {
            button: 0,
            down: true,
        }));
        q.push_back(QueuedCaptureEvent::CursorPoint { x: 2, y: 2 });
        drop_one_for_overflow(&mut q);
        assert!(
            matches!(q.front(), Some(QueuedCaptureEvent::Event(LocalInputEvent::Key { .. }))),
            "front transition kept"
        );
        assert_eq!(
            q.iter().filter(|e| queued_capture_event_is_cursor(e)).count(),
            1,
            "exactly one cursor evicted"
        );

        // With no cursor to sacrifice, fall back to dropping the oldest entry.
        let mut all_transitions: VecDeque<QueuedCaptureEvent> = VecDeque::new();
        all_transitions.push_back(QueuedCaptureEvent::Event(key(0x04, true)));
        all_transitions.push_back(QueuedCaptureEvent::Event(key(0x05, true)));
        drop_one_for_overflow(&mut all_transitions);
        assert_eq!(all_transitions.len(), 1);
    }

    #[test]
    fn raw_mouse_hook_points_are_converted_by_worker() {
        let _guard = capture_test_lock();
        clear_capture_state();
        lock_recover(capture_state()).displays = vec![DisplayBounds {
            id: 3,
            left: 100,
            top: 200,
            width: 500,
            height: 400,
        }];
        let sink = Arc::new(RecordingSink::default());
        install_test_sink(&sink);

        enqueue_cursor_capture_point(125, 250);
        drain_capture_queue_for_test();

        assert_eq!(
            sink.events(),
            vec![LocalInputEvent::CursorMoved {
                display_id: 3,
                x: 25,
                y: 50,
            }]
        );
        clear_capture_state();
    }
}
