//! macOS adapters implementing the `mouser_core` platform traits (audit H2/H3).
//!
//! - [`MacInjector`] implements `mouser_core::InputInjection`. The free
//!   functions in [`crate::inject`] are its private bodies; it adds the
//!   `display_id` → global-coordinate translation (audit M1) and threads the
//!   wire `mods`/`ScrollUnit` through.
//! - [`MacCapture`] implements `mouser_core::InputCapture`. It installs a
//!   **default** (suppress-capable) `CGEventTap` on a background run loop and
//!   honors the [`CaptureDecision`] the sink returns: `Suppress` drops the event
//!   locally, `PassThrough` keeps it. Real suppression needs an active
//!   `CGEventTap` backed by **Accessibility**; when that tap can't be created the
//!   adapter falls back to listen-only and reports `can_suppress() == false`
//!   instead of pretending it swallowed input.

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use core_foundation::runloop::{kCFRunLoopCommonModes, CFRunLoop};
use core_graphics::event::{
    CGEvent, CGEventTap, CGEventTapLocation, CGEventTapOptions, CGEventTapPlacement, CGEventType,
    CallbackResult, EventField,
};
use mouser_core::platform::{
    CaptureDecision, InputCapture, InputInjection, InputSink, LocalInputEvent, PlatformError,
    PlatformResult, ScrollUnit,
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

impl MacCapture {
    /// A not-yet-started capture handle.
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(CaptureRun {
                run_loop: None,
                can_suppress: false,
            })),
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
fn decision_for(sink: &dyn InputSink, ev: LocalInputEvent) -> CallbackResult {
    match sink.on_event(ev) {
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
) -> CallbackResult {
    let keycode = event.get_integer_value_field(EventField::KEYBOARD_EVENT_KEYCODE);
    let resolved = u16::try_from(keycode).ok().and_then(|kc| {
        let mut guard = lock_recover(state);
        flags_changed_event(kc, &mut guard)
    });
    match resolved {
        Some((usage, down)) => decision_for(
            sink,
            LocalInputEvent::Key {
                usage,
                down,
                mods: 0,
            },
        ),
        None => CallbackResult::Keep,
    }
}

/// Build the tap callback: forward to the sink, honor its [`CaptureDecision`].
///
/// Modifier-key (`FlagsChanged`) transitions are stateful — down vs up is the
/// toggle of the changed key's per-keyCode held state, tracked here per-capture
/// in a [`ModifierState`] behind a `Mutex` (the tap callback is `Fn`, so it needs
/// interior mutability).
fn make_callback(
    sink: Arc<dyn InputSink>,
) -> impl Fn(core_graphics::event::CGEventTapProxy, CGEventType, &CGEvent) -> CallbackResult
       + Send
       + 'static {
    let mod_state = Arc::new(Mutex::new(ModifierState::new()));
    move |_proxy, etype, event| {
        if matches!(etype, CGEventType::FlagsChanged) {
            return handle_flags_changed(sink.as_ref(), event, &mod_state);
        }
        match to_local_event(etype, event) {
            Some(ev) => decision_for(sink.as_ref(), ev),
            // Events we don't model are always passed through.
            None => CallbackResult::Keep,
        }
    }
}

impl InputCapture for MacCapture {
    fn start(&self, sink: Arc<dyn InputSink>) -> PlatformResult<()> {
        // Idempotent: already running.
        if lock_recover(&self.inner).run_loop.is_some() {
            return Ok(());
        }

        let inner = Arc::clone(&self.inner);
        let (tx, rx) = std::sync::mpsc::channel::<Result<bool, ()>>();

        std::thread::Builder::new()
            .name("mouser-mac-capture".into())
            .spawn(move || {
                // Prefer a default (suppress-capable) tap; fall back to
                // listen-only if it can't be created (missing Accessibility).
                let (tap, can_suppress) = match CGEventTap::new(
                    CGEventTapLocation::Session,
                    CGEventTapPlacement::HeadInsertEventTap,
                    CGEventTapOptions::Default,
                    events_of_interest(),
                    make_callback(Arc::clone(&sink)),
                ) {
                    Ok(t) => (Some(t), true),
                    Err(()) => match CGEventTap::new(
                        CGEventTapLocation::Session,
                        CGEventTapPlacement::HeadInsertEventTap,
                        CGEventTapOptions::ListenOnly,
                        events_of_interest(),
                        make_callback(sink),
                    ) {
                        Ok(t) => (Some(t), false),
                        Err(()) => (None, false),
                    },
                };

                let Some(tap) = tap else {
                    let _ = tx.send(Err(()));
                    return;
                };

                let Ok(source) = tap.mach_port().create_runloop_source(0) else {
                    let _ = tx.send(Err(()));
                    return;
                };
                let run_loop = CFRunLoop::get_current();
                // SAFETY: `kCFRunLoopCommonModes` is a CoreFoundation extern
                // constant string; reading it is the documented usage.
                run_loop.add_source(&source, unsafe { kCFRunLoopCommonModes });
                tap.enable();

                {
                    let mut g = lock_recover(&inner);
                    g.run_loop = Some(run_loop.clone());
                    g.can_suppress = can_suppress;
                }
                let _ = tx.send(Ok(can_suppress));

                // Blocks until `stop()` calls `CFRunLoop::stop`.
                CFRunLoop::run_current();

                // Run loop ended: clear shared state and drop the tap.
                lock_recover(&inner).run_loop = None;
                drop(tap);
            })
            .map_err(|e| -> PlatformError { Box::new(e) })?;

        match rx.recv() {
            Ok(Ok(_)) => Ok(()),
            _ => Err(Box::new(CaptureStartFailed)),
        }
    }

    fn stop(&self) -> PlatformResult<()> {
        let run_loop = lock_recover(&self.inner).run_loop.take();
        if let Some(rl) = run_loop {
            rl.stop();
        }
        Ok(())
    }

    fn can_suppress(&self) -> bool {
        lock_recover(&self.inner).can_suppress
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
mod tests {
    use super::*;

    /// Sink that records every event and replies with a fixed decision.
    struct RecordingSink {
        decision: CaptureDecision,
        seen: Mutex<Vec<LocalInputEvent>>,
    }

    impl RecordingSink {
        fn new(decision: CaptureDecision) -> Self {
            Self {
                decision,
                seen: Mutex::new(Vec::new()),
            }
        }
    }

    impl InputSink for RecordingSink {
        fn on_event(&self, event: LocalInputEvent) -> CaptureDecision {
            self.seen.lock().expect("seen").push(event);
            self.decision
        }
    }

    #[test]
    fn suppress_decision_drops_the_event() {
        // A modifier Key the engine wants swallowed maps to a tap Drop, and the
        // sink actually saw the event.
        let sink = RecordingSink::new(CaptureDecision::Suppress);
        let key = LocalInputEvent::Key {
            usage: 0xE3,
            down: true,
            mods: 0,
        };
        assert!(matches!(decision_for(&sink, key), CallbackResult::Drop));
        assert_eq!(sink.seen.lock().expect("seen").as_slice(), &[key]);
    }

    #[test]
    fn passthrough_decision_keeps_the_event() {
        let sink = RecordingSink::new(CaptureDecision::PassThrough);
        let key = LocalInputEvent::Key {
            usage: 0xE0,
            down: false,
            mods: 0,
        };
        assert!(matches!(decision_for(&sink, key), CallbackResult::Keep));
    }

    #[test]
    fn lock_recover_recovers_a_poisoned_mutex() {
        // Poison a mutex by panicking while its guard is held, then prove the
        // runtime lock helper still hands back the (intact) data instead of
        // propagating the panic — capture must never abort on a poisoned lock.
        let m = Arc::new(Mutex::new(7_u32));
        let m2 = Arc::clone(&m);
        let _ = std::thread::spawn(move || {
            let _g = m2.lock().expect("acquire to poison");
            panic!("poison the mutex");
        })
        .join();
        assert!(m.lock().is_err(), "mutex should be poisoned");
        // The helper recovers rather than panics, and the value is unchanged.
        assert_eq!(*lock_recover(&m), 7);
    }

    #[test]
    fn capture_control_path_survives_poison() {
        // Poison the real capture mutex (panic while holding its guard), then
        // prove the InputCapture control path (can_suppress / stop) recovers
        // instead of panicking — a poisoned lock must never abort capture.
        let cap = MacCapture::new();
        let inner = Arc::clone(&cap.inner);
        let _ = std::thread::spawn(move || {
            let _g = inner.lock().expect("acquire to poison");
            panic!("poison the capture mutex");
        })
        .join();
        assert!(
            cap.inner.lock().is_err(),
            "capture mutex should be poisoned"
        );
        // Both go through `lock_recover` and must return, not unwind.
        assert!(!cap.can_suppress());
        assert!(cap.stop().is_ok());
    }

    #[test]
    fn unknown_display_id_is_an_error() {
        // `u32::MAX` is never a valid CG display id, so the injector reports an
        // error (not a warp to the wrong place) instead of falling back to the
        // main display (audit M1).
        let inj = MacInjector::new();
        let err = inj.move_cursor(u32::MAX, 0, 0).unwrap_err();
        assert!(err.downcast_ref::<UnknownDisplay>().is_some());
    }
}
