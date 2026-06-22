//! Windows adapter implementing `mouser_core` platform traits.
//!
//! [`WinInjector`] is the engine-facing wrapper over [`crate::inject`]. It maps
//! the platform-neutral trait contract (display-local cursor coordinates, wire
//! button ids, modifier bitmasks, and core scroll units) to the Win32 `SendInput`
//! backend.
//!
//! [`WinCapture`] installs low-level keyboard and mouse hooks on a background
//! thread and forwards modeled local input to `mouser_core::InputSink`. The hook
//! callbacks honor `CaptureDecision::Suppress`, which is the Windows equivalent
//! of the macOS default event tap behavior.

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};
use std::thread::JoinHandle;

use mouser_core::platform::{
    CaptureDecision, InputCapture, InputInjection, InputSink, LocalInputEvent, PlatformError,
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

/// Shared mutable state used by the static Win32 hook callbacks.
#[derive(Default)]
struct CaptureState {
    sink: Option<Arc<dyn InputSink>>,
    displays: Vec<DisplayBounds>,
    mods: u16,
}

static CAPTURE_STATE: OnceLock<Mutex<CaptureState>> = OnceLock::new();

fn capture_state() -> &'static Mutex<CaptureState> {
    CAPTURE_STATE.get_or_init(|| Mutex::new(CaptureState::default()))
}

fn lock_recover<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

fn clear_capture_state() {
    let mut state = lock_recover(capture_state());
    state.sink = None;
    state.displays.clear();
    state.mods = 0;
}

fn prepare_capture_state(sink: Arc<dyn InputSink>) -> PlatformResult<()> {
    let mut state = lock_recover(capture_state());
    if state.sink.is_some() {
        return Err(boxed(CaptureAlreadyRunning));
    }
    state.displays = active_display_bounds().unwrap_or_else(|_| Vec::new());
    state.mods = 0;
    state.sink = Some(sink);
    Ok(())
}

/// Windows input capture via `WH_KEYBOARD_LL` and `WH_MOUSE_LL`.
pub struct WinCapture {
    inner: Arc<Mutex<CaptureRun>>,
}

struct CaptureRun {
    thread_id: Option<u32>,
    can_suppress: bool,
    handle: Option<JoinHandle<()>>,
}

impl Default for WinCapture {
    fn default() -> Self {
        Self::new()
    }
}

impl WinCapture {
    /// A not-yet-started Windows capture handle.
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(CaptureRun {
                thread_id: None,
                can_suppress: false,
                handle: None,
            })),
        }
    }

    fn join_stale_thread(&self) {
        let handle = {
            let mut run = lock_recover(&self.inner);
            if run.thread_id.is_some() {
                return;
            }
            run.handle.take()
        };
        if let Some(handle) = handle {
            let _ = handle.join();
        }
    }
}

impl InputCapture for WinCapture {
    fn start(&self, sink: Arc<dyn InputSink>) -> PlatformResult<()> {
        self.join_stale_thread();
        if lock_recover(&self.inner).thread_id.is_some() {
            return Ok(());
        }

        prepare_capture_state(sink)?;

        let inner = Arc::clone(&self.inner);
        let (tx, rx) = std::sync::mpsc::channel::<Result<(), String>>();
        let handle = std::thread::Builder::new()
            .name("mouser-win-capture".into())
            .spawn(move || run_capture_thread(inner, tx))
            .map_err(boxed)?;

        match rx.recv() {
            Ok(Ok(())) => {
                lock_recover(&self.inner).handle = Some(handle);
                Ok(())
            }
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

    fn stop(&self) -> PlatformResult<()> {
        let thread_id = lock_recover(&self.inner).thread_id;
        let Some(thread_id) = thread_id else {
            self.join_stale_thread();
            clear_capture_state();
            return Ok(());
        };

        // SAFETY: `thread_id` was reported by the capture thread after creating
        // its User32 message queue. Posting WM_QUIT asks its GetMessageW loop to
        // unwind and unhook.
        unsafe { PostThreadMessageW(thread_id, WM_QUIT, WPARAM(0), LPARAM(0)) }.map_err(boxed)?;

        let handle = {
            let mut run = lock_recover(&self.inner);
            run.can_suppress = false;
            run.thread_id = None;
            run.handle.take()
        };

        // Avoid self-joining if stop is ever called from inside the hook sink.
        if unsafe { GetCurrentThreadId() } != thread_id {
            if let Some(handle) = handle {
                let _ = handle.join();
            }
        }
        Ok(())
    }

    fn can_suppress(&self) -> bool {
        lock_recover(&self.inner).can_suppress
    }
}

fn run_capture_thread(
    inner: Arc<Mutex<CaptureRun>>,
    tx: std::sync::mpsc::Sender<Result<(), String>>,
) {
    // SAFETY: thread id is a pure OS query.
    let thread_id = unsafe { GetCurrentThreadId() };
    create_message_queue();

    // SAFETY: low-level hook procedures are static `extern "system"` functions
    // with the exact signature Windows requires. For WH_*_LL hooks the callback
    // runs in this installing thread's context while its message loop is alive.
    let keyboard_hook =
        match unsafe { SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard_hook), None, 0) } {
            Ok(hook) => hook,
            Err(e) => {
                clear_capture_state();
                let _ = tx.send(Err(format!("WH_KEYBOARD_LL install failed: {e}")));
                return;
            }
        };

    // SAFETY: see keyboard hook install above.
    let mouse_hook = match unsafe { SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_hook), None, 0) } {
        Ok(hook) => hook,
        Err(e) => {
            // SAFETY: `keyboard_hook` was returned by SetWindowsHookExW above
            // and is still owned by this thread.
            let _ = unsafe { UnhookWindowsHookEx(keyboard_hook) };
            clear_capture_state();
            let _ = tx.send(Err(format!("WH_MOUSE_LL install failed: {e}")));
            return;
        }
    };

    {
        let mut run = lock_recover(&inner);
        run.thread_id = Some(thread_id);
        run.can_suppress = true;
    }
    let _ = tx.send(Ok(()));

    message_loop();

    // SAFETY: these handles were installed on this thread and have not been
    // unhooked yet. Ignore shutdown failures; capture is already stopping.
    let _ = unsafe { UnhookWindowsHookEx(mouse_hook) };
    let _ = unsafe { UnhookWindowsHookEx(keyboard_hook) };
    clear_capture_state();

    let mut run = lock_recover(&inner);
    run.thread_id = None;
    run.can_suppress = false;
}

fn create_message_queue() {
    let mut msg = MSG::default();
    // SAFETY: a zero-filter PeekMessageW creates this thread's User32 message
    // queue if it does not already exist. PM_NOREMOVE leaves queued messages
    // untouched, which matters because stop() later uses PostThreadMessageW.
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
    let decision = keyboard_event_from_parts(message, hook.scanCode, hook.flags.0)
        .map(dispatch_capture_event)
        .unwrap_or(CaptureDecision::PassThrough);
    hook_result(decision, code, wparam, lparam)
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
    let decision =
        mouse_event_from_parts(message, hook.pt.x, hook.pt.y, hook.mouseData, hook.flags)
            .map(dispatch_capture_event)
            .unwrap_or(CaptureDecision::PassThrough);
    hook_result(decision, code, wparam, lparam)
}

fn call_next(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    // SAFETY: forwarding with a null hook handle is the documented low-level
    // hook pattern; `code/wparam/lparam` are the values Windows supplied.
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

fn hook_result(decision: CaptureDecision, code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match decision {
        CaptureDecision::PassThrough => call_next(code, wparam, lparam),
        CaptureDecision::Suppress => LRESULT(1),
    }
}

fn hook_message(wparam: WPARAM) -> Option<u32> {
    u32::try_from(wparam.0).ok()
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
    let mut state = lock_recover(capture_state());
    if let Some(bit) = modifier_bit(usage) {
        if down {
            state.mods |= bit;
        } else {
            state.mods &= !bit;
        }
    }
    state.mods
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

fn cursor_event_for_virtual_point(x: i32, y: i32) -> LocalInputEvent {
    let displays = lock_recover(capture_state()).displays.clone();
    let bounds = displays
        .iter()
        .copied()
        .find(|b| b.contains_virtual(x, y))
        .or_else(|| displays.iter().copied().next());

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

fn dispatch_capture_event(event: LocalInputEvent) -> CaptureDecision {
    let sink = lock_recover(capture_state()).sink.clone();
    let Some(sink) = sink else {
        return CaptureDecision::PassThrough;
    };
    catch_unwind(AssertUnwindSafe(|| sink.on_event(event))).unwrap_or(CaptureDecision::PassThrough)
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

    static CAPTURE_TEST_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

    fn capture_test_lock() -> MutexGuard<'static, ()> {
        lock_recover(CAPTURE_TEST_LOCK.get_or_init(|| Mutex::new(())))
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
}
