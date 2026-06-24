//! macOS adapters implementing the `mouser_core` platform traits (audit H2/H3).
//!
//! - [`MacInjector`] wraps [`crate::inject`] and adds display-local to global
//!   coordinate translation (audit M1).
//! - [`MacCapture`] installs a background `CGEventTap`, honors
//!   [`CaptureDecision`], and falls back to listen-only when Accessibility is
//!   missing so `can_suppress() == false` is honest.

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use core_foundation::base::TCFType;
use core_foundation::runloop::{kCFRunLoopCommonModes, CFRunLoop};
use core_graphics::event::{
    CGEvent, CGEventTap, CGEventTapLocation, CGEventTapOptions, CGEventTapPlacement, CGEventType,
    CallbackResult, EventField,
};
use mouser_core::platform::{
    CaptureDecision, CaptureMode, InputCapture, InputInjection, InputSink, LocalInputEvent,
    PlatformError, PlatformResult, ScrollUnit,
};

use crate::display_info::display_bounds;
use crate::inject;
use crate::keymap_capture::{flags_changed_event, to_local_event, ModifierState};

/// macOS input injector. Stateless; every call posts a fresh `CGEvent`.
///
/// `cmd_ctrl_swap` is the cluster input preference (Appendix A `input_prefs`);
/// when set, a remote machine's Ctrl is delivered as ⌘ and vice-versa.
#[derive(Debug, Default, Clone, Copy)]
pub struct MacInjector {
    cmd_ctrl_swap: bool,
}

impl MacInjector {
    /// Injector with the default (no) Cmd↔Ctrl swap.
    #[must_use]
    pub fn new() -> Self {
        Self {
            cmd_ctrl_swap: false,
        }
    }

    /// Injector with the Cmd↔Ctrl swap preference set.
    #[must_use]
    pub fn with_cmd_ctrl_swap(cmd_ctrl_swap: bool) -> Self {
        Self { cmd_ctrl_swap }
    }
}

fn boxed(e: inject::InjectError) -> PlatformError {
    Box::new(e)
}

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

/// Error when a wire `display_id` matches no active display.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnknownDisplay(pub u32);

impl std::fmt::Display for UnknownDisplay {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "no active display with id {}", self.0)
    }
}

impl std::error::Error for UnknownDisplay {}

impl InputInjection for MacInjector {
    fn move_cursor(&self, display_id: u32, x: i32, y: i32) -> PlatformResult<()> {
        // Translate display-local logical pixels to a global CG point via full
        // display enumeration (audit M1), not just the main display.
        let bounds = display_bounds(display_id)
            .ok_or_else(|| -> PlatformError { Box::new(UnknownDisplay(display_id)) })?;
        let (gx, gy) = bounds.local_to_global(x, y);
        inject::move_cursor(gx, gy).map_err(boxed)
    }

    fn move_cursor_relative(&self, dx: i32, dy: i32) -> PlatformResult<()> {
        inject::move_cursor_rel(dx, dy).map_err(boxed)
    }

    fn button(&self, button: u8, down: bool) -> PlatformResult<()> {
        inject::button(button, down).map_err(boxed)
    }

    fn key(&self, usage: u16, down: bool, mods: u16) -> PlatformResult<()> {
        inject::key_press(usage, down, mods, self.cmd_ctrl_swap).map_err(boxed)
    }

    fn scroll(&self, dx: i32, dy: i32, unit: ScrollUnit) -> PlatformResult<()> {
        // `Detent120` is line/notch-based; `LogicalPixel` is pixel-precise.
        let pixel = matches!(unit, ScrollUnit::LogicalPixel);
        let (dx, dy) = match unit {
            ScrollUnit::Detent120 => (dx / 120, dy / 120),
            ScrollUnit::LogicalPixel => (dx, dy),
        };
        inject::scroll(dx, dy, pixel).map_err(boxed)
    }
}

/// Shared state between [`MacCapture`] and its run-loop thread.
struct CaptureRun {
    /// The thread's run loop, so [`MacCapture::stop`] can stop it. `CFRunLoop`
    /// is `Send + Sync` (CoreFoundation run-loop fns are thread-safe).
    run_loop: Option<CFRunLoop>,
    /// Whether the installed tap can actually suppress (default tap created).
    can_suppress: bool,
    /// The capture mode the adapter is currently in.
    mode: CaptureMode,
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
                can_suppress: false,
                mode: CaptureMode::Off,
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
    ) -> PlatformResult<()> {
        let inner = Arc::clone(&self.inner);
        // Claim a fresh generation. Any earlier run-loop thread now holds a stale
        // generation and will neither claim nor clear the shared state.
        let my_gen = {
            let mut g = lock_recover(&inner);
            g.generation = g.generation.wrapping_add(1);
            g.generation
        };
        let (tx, rx) = std::sync::mpsc::channel::<Result<bool, ()>>();
        // Shared mach-port ref so the callback can re-enable the tap if macOS disables it.
        // Published once below, after the tap exists.
        let reenable_port = Arc::new(AtomicUsize::new(0));
        let cb_port = Arc::clone(&reenable_port);

        std::thread::Builder::new()
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

                {
                    let mut g = lock_recover(&inner);
                    // Only claim the shared state if a newer tap hasn't superseded us.
                    if g.generation == my_gen {
                        g.run_loop = Some(run_loop.clone());
                        g.can_suppress = can_suppress;
                        g.mode = mode;
                    }
                }
                let _ = tx.send(Ok(can_suppress));

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
            })
            .map_err(|e| -> PlatformError { Box::new(e) })?;

        match rx.recv() {
            Ok(Ok(_)) => Ok(()),
            _ => Err(Box::new(CaptureStartFailed)),
        }
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
        let old_run_loop = {
            let mut g = lock_recover(&self.inner);
            if g.mode == mode {
                return Ok(()); // already in the requested mode
            }
            g.mode = CaptureMode::Off;
            g.can_suppress = false;
            g.run_loop.take()
        };
        if let Some(rl) = old_run_loop {
            rl.stop();
        }
        match mode {
            CaptureMode::Off => Ok(()),
            // PassiveEdge: a listen-only tap senses the edge without ever suppressing.
            CaptureMode::PassiveEdge => self.start_tap(Arc::clone(sink), true, mode),
            // ActiveForward: a default (suppress-capable) tap while driving the peer.
            CaptureMode::ActiveForward => self.start_tap(Arc::clone(sink), false, mode),
        }
    }

    fn stop(&self) -> PlatformResult<()> {
        let run_loop = {
            let mut g = lock_recover(&self.inner);
            g.mode = CaptureMode::Off;
            g.can_suppress = false;
            g.run_loop.take()
        };
        if let Some(rl) = run_loop {
            rl.stop();
        }
        Ok(())
    }

    fn can_suppress(&self) -> bool {
        lock_recover(&self.inner).can_suppress
    }

    fn current_mode(&self) -> CaptureMode {
        lock_recover(&self.inner).mode
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
