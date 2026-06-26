//! macOS adapters implementing the `mouser_core` platform traits (audit H2/H3).
//!
//! - [`MacInjector`] wraps [`crate::injector`] and adds display-local to global
//!   coordinate translation plus cursor visibility.
//! - [`MacCapture`] installs a background `CGEventTap`, honors
//!   [`CaptureDecision`], and falls back to listen-only when Accessibility is
//!   missing so `can_suppress() == false` is honest.

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use core_foundation::base::TCFType;
use core_foundation::runloop::{kCFRunLoopCommonModes, CFRunLoop};
use core_graphics::event::{
    CGEvent, CGEventTap, CGEventTapLocation, CGEventTapOptions, CGEventTapPlacement, CGEventType,
    CallbackResult, EventField,
};
use mouser_core::platform::{
    CaptureDecision, CaptureMode, InputCapture, InputSink, LocalInputEvent, PlatformError,
    PlatformResult,
};

pub use crate::injector::MacInjector;

use crate::keymap_capture::{
    cursor_moved_for_global, flags_changed_event, to_local_event, ModifierState,
};

const PASSIVE_POLL_INTERVAL: Duration = Duration::from_millis(8);

/// Lock a capture mutex on a **runtime** path without ever panicking (audit:
/// capture panic discipline).
///
/// A `Mutex` poisons when a thread panics while holding it. The data these locks
/// protect ([`CaptureRun`], [`ModifierState`]) carries **no broken invariant**
/// after such a panic — it is a plain run-loop handle / a per-keyCode held-state
/// map — so the only correct response on the event-tap and control paths is to
/// recover the guard via [`PoisonError::into_inner`] rather than `.expect(...)`,
/// which would abort capture (the tap callback unwinding across the FFI boundary
/// is undefined behavior). Behavior on the happy (unpoisoned) path is identical.
fn lock_recover<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

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

#[derive(Debug, Default)]
struct EmergencyReclaim {
    held_ctrl_bits: u16,
    active: bool,
}

impl EmergencyReclaim {
    fn new() -> Self {
        Self::default()
    }

    fn observe(&mut self, event: LocalInputEvent) -> bool {
        let was_active = self.active;
        if let LocalInputEvent::Key { usage, down, mods } = event {
            self.update_ctrl(usage, down);
            if emergency_reclaim_chord_from_mods(mods | self.held_ctrl_bits) {
                self.active = true;
            }
        }
        let force_pass = was_active || self.active;
        if self.active && self.held_ctrl_bits == 0 {
            self.active = false;
        }
        force_pass
    }

    fn update_ctrl(&mut self, usage: u16, down: bool) {
        let Some(bit) = ctrl_bit(usage) else {
            return;
        };
        if down {
            self.held_ctrl_bits |= bit;
        } else {
            self.held_ctrl_bits &= !bit;
        }
    }
}

fn ctrl_bit(usage: u16) -> Option<u16> {
    match usage {
        0xE0 => Some(LEFT_CTRL_BIT),
        0xE4 => Some(RIGHT_CTRL_BIT),
        _ => None,
    }
}

/// Shared state between [`MacCapture`] and its run-loop thread.
struct CaptureRun {
    /// The thread's run loop, so [`MacCapture::stop`] can stop it. `CFRunLoop`
    /// is `Send + Sync` (CoreFoundation run-loop fns are thread-safe).
    run_loop: Option<CFRunLoop>,
    /// Stop flag and thread handle for passive cursor polling.
    passive_stop: Option<Arc<AtomicBool>>,
    passive_handle: Option<JoinHandle<()>>,
    /// Whether the installed tap can actually suppress (default tap created).
    can_suppress: bool,
    /// The capture mode the adapter is currently in.
    mode: CaptureMode,
    /// A mode transition has claimed `generation` but has not published its OS
    /// resources yet. This keeps `set_mode(Off)` from treating an in-flight start
    /// as an idle Off no-op.
    pending_mode: Option<CaptureMode>,
    /// Bumped each time a new tap thread is started. A run-loop thread only
    /// claims/clears the shared state if its captured generation is still current,
    /// so a superseded thread's teardown epilogue can never clobber the run loop a
    /// newer transition just installed (mac does not join the old thread, unlike
    /// the Windows adapter which tears down synchronously).
    generation: u64,
}

/// macOS input capture via a background `CGEventTap` (audit H3).
pub struct MacCapture {
    inner: Arc<Mutex<CaptureRun>>,
}

impl Default for MacCapture {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for MacCapture {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

impl MacCapture {
    /// A not-yet-started capture handle (mode [`CaptureMode::Off`]).
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(CaptureRun {
                run_loop: None,
                passive_stop: None,
                passive_handle: None,
                can_suppress: false,
                mode: CaptureMode::Off,
                pending_mode: None,
                generation: 0,
            })),
        }
    }

    /// Bring a `CGEventTap` up on a background run loop. `listen_only` forces a
    /// non-suppressing tap (the passive edge-sensing mode); otherwise a
    /// suppress-capable default tap is tried first, falling back to listen-only when
    /// Accessibility is missing (so we never pretend to suppress what we cannot).
    fn start_tap(
        &self,
        sink: Arc<dyn InputSink>,
        listen_only: bool,
        mode: CaptureMode,
        my_gen: u64,
        required: bool,
    ) -> PlatformResult<()> {
        let inner = Arc::clone(&self.inner);
        let (tx, rx) = std::sync::mpsc::channel::<Result<bool, ()>>();
        // Shared mach-port ref so the callback can re-enable the tap if macOS disables it.
        // Published once below, after the tap exists.
        let reenable_port = Arc::new(AtomicUsize::new(0));
        let cb_port = Arc::clone(&reenable_port);

        let spawn_result = std::thread::Builder::new()
            .name("mouser-mac-capture".into())
            .spawn(move || {
                let (tap, can_suppress) = if listen_only {
                    // PassiveEdge: observe only, never suppress.
                    match CGEventTap::new(
                        CGEventTapLocation::Session,
                        CGEventTapPlacement::HeadInsertEventTap,
                        CGEventTapOptions::ListenOnly,
                        events_of_interest(),
                        make_callback(Arc::clone(&sink), Arc::clone(&cb_port)),
                    ) {
                        Ok(t) => (Some(t), false),
                        Err(()) => (None, false),
                    }
                } else {
                    // ActiveForward: prefer a suppress-capable default tap; fall back
                    // to listen-only if Accessibility can't be created.
                    match CGEventTap::new(
                        CGEventTapLocation::Session,
                        CGEventTapPlacement::HeadInsertEventTap,
                        CGEventTapOptions::Default,
                        events_of_interest(),
                        make_callback(Arc::clone(&sink), Arc::clone(&cb_port)),
                    ) {
                        Ok(t) => (Some(t), true),
                        Err(()) => match CGEventTap::new(
                            CGEventTapLocation::Session,
                            CGEventTapPlacement::HeadInsertEventTap,
                            CGEventTapOptions::ListenOnly,
                            events_of_interest(),
                            make_callback(Arc::clone(&sink), Arc::clone(&cb_port)),
                        ) {
                            Ok(t) => (Some(t), false),
                            Err(()) => (None, false),
                        },
                    }
                };

                let Some(tap) = tap else {
                    let _ = tx.send(Err(()));
                    return;
                };

                let Ok(source) = tap.mach_port().create_runloop_source(0) else {
                    let _ = tx.send(Err(()));
                    return;
                };
                if lock_recover(&inner).generation != my_gen {
                    let _ = tx.send(Ok(can_suppress));
                    return;
                }
                // Publish the port before enabling so a disable event delivered immediately
                // after enable() is recoverable by the callback.
                cb_port.store(
                    tap.mach_port().as_concrete_TypeRef() as usize,
                    Ordering::Release,
                );
                let run_loop = CFRunLoop::get_current();
                // SAFETY: `kCFRunLoopCommonModes` is a CoreFoundation extern
                // constant string; reading it is the documented usage.
                run_loop.add_source(&source, unsafe { kCFRunLoopCommonModes });
                tap.enable();

                let stale = {
                    let mut g = lock_recover(&inner);
                    // Only claim the shared state if a newer tap hasn't superseded us.
                    if g.generation == my_gen {
                        g.run_loop = Some(run_loop.clone());
                        g.can_suppress = can_suppress;
                        g.mode = mode;
                        g.pending_mode = None;
                        false
                    } else {
                        true
                    }
                };
                let _ = tx.send(Ok(can_suppress));
                if stale {
                    return;
                }

                // Blocks until `stop()` calls `CFRunLoop::stop`.
                CFRunLoop::run_current();

                // Run loop ended: clear only the state we still own, so a teardown
                // epilogue can't wipe a run loop a newer transition just installed.
                let mut g = lock_recover(&inner);
                if g.generation == my_gen {
                    g.run_loop = None;
                    g.can_suppress = false;
                }
                drop(tap);
            });
        if let Err(e) = spawn_result {
            if required {
                let mut g = lock_recover(&self.inner);
                clear_pending_start(&mut g, my_gen);
                return Err(Box::new(e) as PlatformError);
            }
            return Ok(());
        }

        match rx.recv() {
            Ok(Ok(_)) => Ok(()),
            _ if required => {
                let mut g = lock_recover(&self.inner);
                if g.generation == my_gen {
                    g.pending_mode = None;
                    g.mode = CaptureMode::Off;
                    g.can_suppress = false;
                }
                Err(Box::new(CaptureStartFailed))
            }
            _ => Ok(()),
        }
    }

    /// Passive edge sensing uses cursor polling plus an optional listen-only tap.
    ///
    /// Polling is the no-hook fallback and catches observable absolute cursor
    /// changes. The listen-only tap, when macOS permits it, supplies true device
    /// deltas while the pointer is clamped at an edge, so a continued push through
    /// the left/right/top/bottom edge can still trigger the predictive crossing
    /// logic.
    fn start_passive_edge(&self, sink: Arc<dyn InputSink>, my_gen: u64) -> PlatformResult<()> {
        self.start_passive_poll(Arc::clone(&sink), my_gen)?;
        let _ = self.start_tap(sink, true, CaptureMode::PassiveEdge, my_gen, false);
        Ok(())
    }

    fn start_passive_poll(&self, sink: Arc<dyn InputSink>, my_gen: u64) -> PlatformResult<()> {
        let stop = Arc::new(AtomicBool::new(false));
        {
            let mut g = lock_recover(&self.inner);
            if g.generation != my_gen {
                return Ok(());
            }
            g.passive_stop = Some(Arc::clone(&stop));
        }
        let worker_stop = Arc::clone(&stop);
        let handle = std::thread::Builder::new()
            .name("mouser-mac-passive-edge".into())
            .spawn(move || passive_poll_thread(sink, worker_stop))
            .map_err(|e| {
                let mut g = lock_recover(&self.inner);
                if g.generation == my_gen {
                    g.passive_stop = None;
                    g.passive_handle = None;
                }
                clear_pending_start(&mut g, my_gen);
                Box::new(e) as PlatformError
            })?;

        let mut g = lock_recover(&self.inner);
        if g.generation == my_gen {
            g.passive_stop = Some(stop);
            g.passive_handle = Some(handle);
            g.can_suppress = false;
            g.mode = CaptureMode::PassiveEdge;
            g.pending_mode = None;
            return Ok(());
        }
        drop(g);
        stop_detached(None, Some(stop), Some(handle));
        Ok(())
    }

    fn take_current(
        &self,
    ) -> (
        Option<CFRunLoop>,
        Option<Arc<AtomicBool>>,
        Option<JoinHandle<()>>,
    ) {
        let mut g = lock_recover(&self.inner);
        bump_generation(&mut g);
        g.mode = CaptureMode::Off;
        g.pending_mode = None;
        g.can_suppress = false;
        (
            g.run_loop.take(),
            g.passive_stop.take(),
            g.passive_handle.take(),
        )
    }
}

fn bump_generation(g: &mut CaptureRun) -> u64 {
    g.generation = g.generation.wrapping_add(1);
    g.generation
}

fn clear_pending_start(g: &mut CaptureRun, generation: u64) {
    if g.generation == generation {
        g.pending_mode = None;
        g.mode = CaptureMode::Off;
        g.can_suppress = false;
    }
}

fn stop_detached(
    run_loop: Option<CFRunLoop>,
    passive_stop: Option<Arc<AtomicBool>>,
    passive_handle: Option<JoinHandle<()>>,
) {
    if let Some(rl) = run_loop {
        rl.stop();
    }
    if let Some(stop) = passive_stop {
        stop.store(true, Ordering::Release);
    }
    if let Some(handle) = passive_handle {
        if handle.thread().id() == thread::current().id() {
            return;
        }
        let _ = handle.join();
    }
}

fn passive_poll_thread(sink: Arc<dyn InputSink>, stop: Arc<AtomicBool>) {
    let mut last: Option<(f64, f64)> = None;
    while !stop.load(Ordering::Acquire) {
        if let Some(point) = crate::inject::cursor_position() {
            let pos = (point.x, point.y);
            if last != Some(pos) {
                let (dx, dy) = match last {
                    Some((px, py)) => {
                        ((point.x - px).round() as i32, (point.y - py).round() as i32)
                    }
                    None => (0, 0),
                };
                last = Some(pos);
                let event = cursor_moved_for_global(point.x, point.y, dx, dy);
                let _ = catch_unwind(AssertUnwindSafe(|| sink.on_event(event)));
            }
        }
        std::thread::sleep(PASSIVE_POLL_INTERVAL);
    }
}

/// The mouse/key/scroll event types the capture tap observes.
fn events_of_interest() -> Vec<CGEventType> {
    vec![
        CGEventType::MouseMoved,
        CGEventType::LeftMouseDown,
        CGEventType::LeftMouseUp,
        CGEventType::RightMouseDown,
        CGEventType::RightMouseUp,
        CGEventType::OtherMouseDown,
        CGEventType::OtherMouseUp,
        CGEventType::LeftMouseDragged,
        CGEventType::RightMouseDragged,
        CGEventType::KeyDown,
        CGEventType::KeyUp,
        CGEventType::FlagsChanged,
        CGEventType::ScrollWheel,
    ]
}

/// Hand one modeled event to the sink and translate its [`CaptureDecision`] into
/// the tap's [`CallbackResult`].
fn decision_for(
    sink: &dyn InputSink,
    ev: LocalInputEvent,
    reclaim: &Mutex<EmergencyReclaim>,
) -> CallbackResult {
    let force_pass = lock_recover(reclaim).observe(ev);
    let decision = catch_unwind(AssertUnwindSafe(|| sink.on_event(ev)))
        .unwrap_or(CaptureDecision::PassThrough);
    if force_pass {
        return CallbackResult::Keep;
    }
    match decision {
        CaptureDecision::PassThrough => CallbackResult::Keep,
        CaptureDecision::Suppress => CallbackResult::Drop,
    }
}

/// Resolve a `FlagsChanged` event into a `Key` and forward it to the sink.
///
/// macOS delivers modifier (Ctrl/Shift/Alt/Cmd) transitions as `FlagsChanged`,
/// not KeyDown/KeyUp. Down vs up is derived from the per-keyCode held state in
/// `state` (each `FlagsChanged` for a keyCode toggles it), so the release of one
/// of two held left/right twins is reported correctly even though they share a
/// single device-independent flag bit (audit FlagsChanged-twin). Returns the
/// sink's decision so a suppress-capable tap can drop a modifier press; events we
/// don't model (Fn/CapsLock) pass through and leave `state` untouched.
fn handle_flags_changed(
    sink: &dyn InputSink,
    event: &CGEvent,
    state: &Mutex<ModifierState>,
    reclaim: &Mutex<EmergencyReclaim>,
) -> CallbackResult {
    let keycode = event.get_integer_value_field(EventField::KEYBOARD_EVENT_KEYCODE);
    let resolved = u16::try_from(keycode).ok().and_then(|kc| {
        let mut guard = lock_recover(state);
        flags_changed_event(kc, &mut guard).map(|(usage, down)| LocalInputEvent::Key {
            usage,
            down,
            mods: guard.modifier_bits(),
        })
    });
    match resolved {
        Some(ev) => decision_for(sink, ev, reclaim),
        None => CallbackResult::Keep,
    }
}

// Re-enable a `CGEventTap` by its mach-port ref. macOS disables a tap whose callback
// stalls (`TapDisabledByTimeout`) or under certain user-input bursts
// (`TapDisabledByUserInput`); the tap then delivers nothing until re-enabled, so the
// callback must re-arm it the moment it sees a disable event — otherwise capture goes
// silent for the rest of the session. `CGEventTapEnable` is in CoreGraphics (already
// linked); re-declared here because the `core-graphics` crate keeps it private.
extern "C" {
    fn CGEventTapEnable(tap: *const std::ffi::c_void, enable: bool);
}

/// Build the tap callback: forward to the sink, honor its [`CaptureDecision`].
///
/// Modifier-key (`FlagsChanged`) transitions are stateful — down vs up is the
/// toggle of the changed key's per-keyCode held state, tracked here per-capture
/// in a [`ModifierState`] behind a `Mutex` (the tap callback is `Fn`, so it needs
/// interior mutability).
fn make_callback(
    sink: Arc<dyn InputSink>,
    reenable_port: Arc<AtomicUsize>,
) -> impl Fn(core_graphics::event::CGEventTapProxy, CGEventType, &CGEvent) -> CallbackResult
       + Send
       + 'static {
    let mod_state = Arc::new(Mutex::new(ModifierState::new()));
    let reclaim = Arc::new(Mutex::new(EmergencyReclaim::new()));
    move |_proxy, etype, event| {
        // macOS disabled the tap (callback stall or user-input burst). Re-arm it in place,
        // or the source goes silent until the session ends.
        if matches!(
            etype,
            CGEventType::TapDisabledByTimeout | CGEventType::TapDisabledByUserInput
        ) {
            let port = reenable_port.load(Ordering::Acquire);
            if port != 0 {
                unsafe { CGEventTapEnable(port as *const std::ffi::c_void, true) };
            }
            return CallbackResult::Keep;
        }
        if matches!(etype, CGEventType::FlagsChanged) {
            return handle_flags_changed(sink.as_ref(), event, &mod_state, &reclaim);
        }
        match to_local_event(etype, event) {
            Some(ev) => decision_for(sink.as_ref(), ev, &reclaim),
            // Events we don't model are always passed through.
            None => CallbackResult::Keep,
        }
    }
}

impl InputCapture for MacCapture {
    fn set_mode(&self, mode: CaptureMode, sink: &Arc<dyn InputSink>) -> PlatformResult<()> {
        // Check idempotency and detach the current run loop atomically under one lock
        // (so a same-mode no-op and the stop bookkeeping can't interleave), then stop
        // the old run loop and start the new tap outside the lock. A mode change swaps
        // ListenOnly↔Default; on a start failure we are left stopped (mode Off).
        let (old, my_gen) = {
            let mut g = lock_recover(&self.inner);
            if g.mode == mode && g.pending_mode.is_none() {
                return Ok(()); // already in the requested mode
            }
            if g.pending_mode == Some(mode) {
                return Ok(()); // already starting the requested mode
            }
            let my_gen = bump_generation(&mut g);
            g.mode = CaptureMode::Off;
            g.pending_mode = if mode == CaptureMode::Off {
                None
            } else {
                Some(mode)
            };
            g.can_suppress = false;
            (
                (
                    g.run_loop.take(),
                    g.passive_stop.take(),
                    g.passive_handle.take(),
                ),
                my_gen,
            )
        };
        stop_detached(old.0, old.1, old.2);
        match mode {
            CaptureMode::Off => Ok(()),
            // PassiveEdge: poll plus optional listen-only tap for edge-clamped deltas.
            CaptureMode::PassiveEdge => self.start_passive_edge(Arc::clone(sink), my_gen),
            // ActiveForward: a default (suppress-capable) tap while driving the peer.
            CaptureMode::ActiveForward => {
                self.start_tap(Arc::clone(sink), false, mode, my_gen, true)
            }
        }
    }

    fn stop(&self) -> PlatformResult<()> {
        let current = self.take_current();
        stop_detached(current.0, current.1, current.2);
        Ok(())
    }

    fn can_suppress(&self) -> bool {
        lock_recover(&self.inner).can_suppress
    }

    fn current_mode(&self) -> CaptureMode {
        lock_recover(&self.inner).mode
    }

    fn diagnostics(&self) -> String {
        let g = lock_recover(&self.inner);
        format!(
            "mac_capture: pending={:?} passive_poll={} tap={} can_suppress={}",
            g.pending_mode,
            g.passive_handle.is_some(),
            g.run_loop.is_some(),
            g.can_suppress
        )
    }
}

/// The capture tap could not be created — almost always a missing TCC grant
/// (Accessibility + Input Monitoring). See [`crate::capture`] module docs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaptureStartFailed;

impl std::fmt::Display for CaptureStartFailed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "CGEventTap could not be created — grant Accessibility + Input \
             Monitoring (TCC) and relaunch"
        )
    }
}

impl std::error::Error for CaptureStartFailed {}

#[cfg(test)]
#[path = "adapter_tests.rs"]
mod tests;
