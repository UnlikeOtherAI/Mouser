use std::collections::VecDeque;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, AtomicU16, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock, PoisonError};
use std::thread::JoinHandle;

use mouser_core::platform::{InputSink, LocalInputEvent};
use windows::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, KBDLLHOOKSTRUCT, LLKHF_EXTENDED, LLKHF_INJECTED, LLMHF_INJECTED,
    MSLLHOOKSTRUCT, WM_KEYDOWN, WM_KEYUP, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MBUTTONDOWN,
    WM_MBUTTONUP, WM_MOUSEHWHEEL, WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_RBUTTONDOWN, WM_RBUTTONUP,
    WM_SYSKEYDOWN, WM_SYSKEYUP, WM_XBUTTONDOWN, WM_XBUTTONUP, XBUTTON1, XBUTTON2,
};

use crate::adapter::{active_display_bounds, DisplayBounds};
use crate::capture::{is_emergency_reclaim_event, CaptureAlreadyRunning};
use crate::keymap::scancode_to_hid_usage;

#[derive(Default)]
struct CaptureState {
    sink: Option<Arc<dyn InputSink>>,
    displays: Vec<DisplayBounds>,
}

struct CaptureQueue {
    pending: Mutex<VecDeque<QueuedCaptureEvent>>,
    ready: Condvar,
}

// `RawMouseDelta` is a true relative device delta from Raw Input (`WM_INPUT`); unlike the
// absolute `CursorPoint`, it coalesces by summing (see `enqueue_queued_capture_event`).
#[derive(Clone, Copy)]
enum QueuedCaptureEvent {
    Event(LocalInputEvent),
    CursorPoint { x: i32, y: i32 },
    RawMouseDelta { dx: i32, dy: i32 },
}

const MAX_CAPTURE_QUEUE: usize = 256;

static CAPTURE_STATE: OnceLock<Mutex<CaptureState>> = OnceLock::new();
static CAPTURE_QUEUE: OnceLock<CaptureQueue> = OnceLock::new();
static CAPTURE_MODS: AtomicU16 = AtomicU16::new(0);
static CAPTURE_EMERGENCY_PASSTHROUGH: AtomicBool = AtomicBool::new(false);

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

pub(crate) fn clear_capture_state() {
    CAPTURE_MODS.store(0, Ordering::Release);
    CAPTURE_EMERGENCY_PASSTHROUGH.store(false, Ordering::Release);
    crate::capture_rawinput::set_raw_input_active(false);
    clear_capture_queue();
    reset_last_capture_point();
    let mut state = lock_recover(capture_state());
    state.sink = None;
    state.displays.clear();
}

pub(crate) fn prepare_capture_state(sink: Arc<dyn InputSink>) -> Result<(), CaptureAlreadyRunning> {
    let mut state = lock_recover(capture_state());
    if state.sink.is_some() {
        return Err(CaptureAlreadyRunning);
    }
    CAPTURE_MODS.store(0, Ordering::Release);
    CAPTURE_EMERGENCY_PASSTHROUGH.store(false, Ordering::Release);
    crate::capture_rawinput::set_raw_input_active(false);
    clear_capture_queue();
    reset_last_capture_point();
    state.displays = active_display_bounds().unwrap_or_default();
    state.sink = Some(sink);
    Ok(())
}

pub(crate) fn stop_capture_worker(stop: Arc<AtomicBool>, worker: JoinHandle<()>) {
    stop.store(true, Ordering::Release);
    capture_queue().ready.notify_all();
    let _ = worker.join();
}

pub(crate) fn capture_worker(stop: Arc<AtomicBool>) {
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

pub(crate) unsafe extern "system" fn keyboard_hook(
    code: i32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if code < 0 {
        return call_next(code, wparam, lparam);
    }
    let Some(message) = hook_message(wparam) else {
        return call_next(code, wparam, lparam);
    };
    if lparam.0 == 0 {
        return call_next(code, wparam, lparam);
    }

    let hook = unsafe { *(lparam.0 as *const KBDLLHOOKSTRUCT) };
    if hook.flags.0 & LLKHF_INJECTED.0 != 0 {
        return call_next(code, wparam, lparam);
    }
    if !is_key_message(message) {
        return call_next(code, wparam, lparam);
    }

    let pass_through =
        if let Some(event) = keyboard_event_from_parts(message, hook.scanCode, hook.flags.0) {
            enqueue_capture_event(event);
            observe_emergency_reclaim(event)
        } else {
            emergency_passthrough_active()
        };
    if pass_through {
        call_next(code, wparam, lparam)
    } else {
        LRESULT(1)
    }
}

pub(crate) unsafe extern "system" fn mouse_hook(
    code: i32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if code < 0 {
        return call_next(code, wparam, lparam);
    }
    let Some(message) = hook_message(wparam) else {
        return call_next(code, wparam, lparam);
    };
    if lparam.0 == 0 {
        return call_next(code, wparam, lparam);
    }

    let hook = unsafe { *(lparam.0 as *const MSLLHOOKSTRUCT) };
    if hook.flags & LLMHF_INJECTED != 0 {
        return call_next(code, wparam, lparam);
    }

    if message == WM_MOUSEMOVE {
        // When Raw Input drives motion via relative deltas, the hook only suppresses; the
        // absolute point would clamp at the edge and stall the peer. Enqueue it only when raw
        // is inactive (registration failed) or an absolute device is in use (no raw delta).
        if !crate::capture_rawinput::raw_input_active()
            || crate::capture_rawinput::raw_device_absolute()
        {
            enqueue_cursor_capture_point(hook.pt.x, hook.pt.y);
        }
        return if emergency_passthrough_active() {
            call_next(code, wparam, lparam)
        } else {
            LRESULT(1)
        };
    }
    if let Some(event) = mouse_event_from_parts(message, hook.mouseData, hook.flags) {
        enqueue_capture_event(event);
        return if emergency_passthrough_active() {
            call_next(code, wparam, lparam)
        } else {
            LRESULT(1)
        };
    }
    call_next(code, wparam, lparam)
}

fn observe_emergency_reclaim(event: LocalInputEvent) -> bool {
    if is_emergency_reclaim_event(event) {
        CAPTURE_EMERGENCY_PASSTHROUGH.store(true, Ordering::Release);
    }
    emergency_passthrough_active()
}

fn emergency_passthrough_active() -> bool {
    CAPTURE_EMERGENCY_PASSTHROUGH.load(Ordering::Acquire)
}

/// Enqueue a true relative device delta from Raw Input (sum-coalesced). The raw-input state
/// flags themselves live in [`crate::capture_rawinput`].
pub(crate) fn enqueue_raw_mouse_delta(dx: i32, dy: i32) {
    enqueue_queued_capture_event(QueuedCaptureEvent::RawMouseDelta { dx, dy });
}

fn call_next(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

fn hook_message(wparam: WPARAM) -> Option<u32> {
    u32::try_from(wparam.0).ok()
}

fn is_key_message(message: u32) -> bool {
    matches!(message, WM_KEYDOWN | WM_KEYUP | WM_SYSKEYDOWN | WM_SYSKEYUP)
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

fn mouse_event_from_parts(message: u32, mouse_data: u32, flags: u32) -> Option<LocalInputEvent> {
    if flags & LLMHF_INJECTED != 0 {
        return None;
    }

    // WM_MOUSEMOVE is deliberately absent: `mouse_hook` handles it earlier (enqueued as a
    // coalescing CursorPoint and resolved on the worker thread). Resolving it here would let
    // the hook thread mutate the single-writer LAST_CAPTURE_POINT and corrupt the baseline.
    let btn = |button, down| Some(LocalInputEvent::Button { button, down });
    match message {
        WM_LBUTTONDOWN => btn(0, true),
        WM_LBUTTONUP => btn(0, false),
        WM_RBUTTONDOWN => btn(1, true),
        WM_RBUTTONUP => btn(1, false),
        WM_MBUTTONDOWN => btn(2, true),
        WM_MBUTTONUP => btn(2, false),
        WM_XBUTTONDOWN | WM_XBUTTONUP => {
            x_button(mouse_data).and_then(|button| btn(button, message == WM_XBUTTONDOWN))
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

pub(crate) fn virtual_point_to_event(
    displays: &[DisplayBounds],
    x: i32,
    y: i32,
    dx: i32,
    dy: i32,
) -> LocalInputEvent {
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
                dx,
                dy,
            }
        }
        None => LocalInputEvent::CursorMoved {
            display_id: 0,
            x,
            y,
            dx,
            dy,
        },
    }
}

/// Last dispatched cursor point; the delta carries motion to the peer. Coalescing keeps
/// only the latest absolute point, so computing the delta at dispatch time yields the full
/// accumulated motion since the previous dispatch.
///
/// This is a successive-absolute delta: while the OS cursor is pinned at a screen edge
/// during suppression the absolute point clamps and the delta decays to 0. The edge-pinned
/// ActiveForward source therefore no longer relies on it — it uses Raw Input (`WM_INPUT`)
/// relative deltas (see [`crate::capture_rawinput`]), which keep flowing regardless of where
/// the OS thinks the cursor is. This absolute path remains for PassiveEdge cursor sensing,
/// the absolute-device fallback (tablets/RDP/touch), and when raw registration fails.
static LAST_CAPTURE_POINT: Mutex<Option<(i32, i32)>> = Mutex::new(None);

fn cursor_event_for_virtual_point(x: i32, y: i32) -> LocalInputEvent {
    let displays = lock_recover(capture_state()).displays.clone();
    let (dx, dy) = {
        let mut last = lock_recover(&LAST_CAPTURE_POINT);
        let delta = match *last {
            Some((px, py)) => (x - px, y - py),
            None => (0, 0),
        };
        *last = Some((x, y));
        delta
    };
    virtual_point_to_event(&displays, x, y, dx, dy)
}

fn reset_last_capture_point() {
    *lock_recover(&LAST_CAPTURE_POINT) = None;
}

/// Build a `CursorMoved` from a true relative device delta (`WM_INPUT`): position is the
/// current OS cursor point (pinned near the entry edge by `ClipCursor` during suppression),
/// delta is the real device motion. Deliberately does NOT touch [`LAST_CAPTURE_POINT`] —
/// that baseline belongs to the absolute path and mixing the two would corrupt its delta.
fn raw_delta_event(dx: i32, dy: i32) -> LocalInputEvent {
    let displays = lock_recover(capture_state()).displays.clone();
    let (x, y) = match crate::inject::cursor_position() {
        Ok(point) => (point.x, point.y),
        Err(_) => (0, 0),
    };
    virtual_point_to_event(&displays, x, y, dx, dy)
}

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
        QueuedCaptureEvent::RawMouseDelta { dx, dy } => {
            dispatch_capture_event(raw_delta_event(dx, dy))
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
    let mut pending = lock_recover(&queue.pending);

    // Raw deltas are incremental: a trailing one is extended by SUMMING (dropping it would
    // lose motion). Absolute points carry full position, so a trailing one is replaced.
    if let QueuedCaptureEvent::RawMouseDelta { dx, dy } = event {
        if let Some(QueuedCaptureEvent::RawMouseDelta {
            dx: last_dx,
            dy: last_dy,
        }) = pending.back_mut()
        {
            *last_dx = last_dx.saturating_add(dx);
            *last_dy = last_dy.saturating_add(dy);
            queue.ready.notify_one();
            return;
        }
    } else if queued_capture_event_is_cursor(&event) {
        if let Some(last) = pending.back_mut() {
            if queued_capture_event_is_cursor(last) {
                *last = event;
                queue.ready.notify_one();
                return;
            }
        }
    }

    if pending.len() >= MAX_CAPTURE_QUEUE {
        drop_one_for_overflow(&mut pending);
    }
    pending.push_back(event);
    queue.ready.notify_one();
}

fn drop_one_for_overflow(pending: &mut VecDeque<QueuedCaptureEvent>) {
    if let Some(idx) = pending.iter().position(queued_capture_event_is_motion) {
        let _ = pending.remove(idx);
    } else {
        let _ = pending.pop_front();
    }
}

/// Replace-coalesce predicate (absolute cursor points only; raw deltas sum-coalesce).
fn queued_capture_event_is_cursor(event: &QueuedCaptureEvent) -> bool {
    matches!(
        event,
        QueuedCaptureEvent::CursorPoint { .. }
            | QueuedCaptureEvent::Event(LocalInputEvent::CursorMoved { .. })
    )
}

/// Overflow-eviction predicate: any motion event is droppable before a button/key transition.
fn queued_capture_event_is_motion(event: &QueuedCaptureEvent) -> bool {
    queued_capture_event_is_cursor(event)
        || matches!(event, QueuedCaptureEvent::RawMouseDelta { .. })
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

#[cfg(test)]
pub(crate) static CAPTURE_TEST_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

#[cfg(test)]
pub(crate) fn capture_test_lock() -> MutexGuard<'static, ()> {
    lock_recover(CAPTURE_TEST_LOCK.get_or_init(|| Mutex::new(())))
}

#[cfg(test)]
#[path = "capture_hooks_tests.rs"]
mod tests;
