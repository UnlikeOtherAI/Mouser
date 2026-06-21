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

use std::sync::{Arc, Mutex};

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
use crate::keymap::cgkeycode_to_hid_usage;

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
        Self { cmd_ctrl_swap: false }
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

/// Translate a captured `CGEvent` into a [`LocalInputEvent`], or `None` for event
/// types we don't forward.
///
/// Captured `mods` is reported as `0`: macOS taps don't expose a portable flags
/// getter here, so the engine derives modifier state from the observed modifier
/// **key** transitions (HID 0xE0..=0xE7) it also receives.
fn to_local_event(etype: CGEventType, event: &CGEvent) -> Option<LocalInputEvent> {
    match etype {
        CGEventType::MouseMoved
        | CGEventType::LeftMouseDragged
        | CGEventType::RightMouseDragged
        | CGEventType::OtherMouseDragged => {
            let p = event.location();
            Some(LocalInputEvent::CursorMoved {
                display_id: 0,
                x: p.x as i32,
                y: p.y as i32,
            })
        }
        CGEventType::LeftMouseDown => Some(LocalInputEvent::Button { button: 0, down: true }),
        CGEventType::LeftMouseUp => Some(LocalInputEvent::Button { button: 0, down: false }),
        CGEventType::RightMouseDown => Some(LocalInputEvent::Button { button: 1, down: true }),
        CGEventType::RightMouseUp => Some(LocalInputEvent::Button { button: 1, down: false }),
        CGEventType::OtherMouseDown | CGEventType::OtherMouseUp => {
            let n = event.get_integer_value_field(EventField::MOUSE_EVENT_BUTTON_NUMBER);
            let down = matches!(etype, CGEventType::OtherMouseDown);
            u8::try_from(n).ok().map(|button| LocalInputEvent::Button { button, down })
        }
        CGEventType::KeyDown | CGEventType::KeyUp => {
            let keycode = event.get_integer_value_field(EventField::KEYBOARD_EVENT_KEYCODE);
            let usage = u16::try_from(keycode)
                .ok()
                .and_then(cgkeycode_to_hid_usage)?;
            Some(LocalInputEvent::Key {
                usage,
                down: matches!(etype, CGEventType::KeyDown),
                mods: 0,
            })
        }
        CGEventType::ScrollWheel => {
            let dy = event.get_integer_value_field(EventField::SCROLL_WHEEL_EVENT_DELTA_AXIS_1);
            let dx = event.get_integer_value_field(EventField::SCROLL_WHEEL_EVENT_DELTA_AXIS_2);
            Some(LocalInputEvent::Scroll {
                dx: dx as i32,
                dy: dy as i32,
            })
        }
        _ => None,
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

/// Build the tap callback: forward to the sink, honor its [`CaptureDecision`].
fn make_callback(
    sink: Arc<dyn InputSink>,
) -> impl Fn(core_graphics::event::CGEventTapProxy, CGEventType, &CGEvent) -> CallbackResult + Send + 'static
{
    move |_proxy, etype, event| match to_local_event(etype, event) {
        Some(ev) => match sink.on_event(ev) {
            CaptureDecision::PassThrough => CallbackResult::Keep,
            CaptureDecision::Suppress => CallbackResult::Drop,
        },
        // Events we don't model are always passed through.
        None => CallbackResult::Keep,
    }
}

impl InputCapture for MacCapture {
    fn start(&self, sink: Arc<dyn InputSink>) -> PlatformResult<()> {
        // Idempotent: already running.
        if self.inner.lock().expect("capture mutex").run_loop.is_some() {
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
                    let mut g = inner.lock().expect("capture mutex");
                    g.run_loop = Some(run_loop.clone());
                    g.can_suppress = can_suppress;
                }
                let _ = tx.send(Ok(can_suppress));

                // Blocks until `stop()` calls `CFRunLoop::stop`.
                CFRunLoop::run_current();

                // Run loop ended: clear shared state and drop the tap.
                inner.lock().expect("capture mutex").run_loop = None;
                drop(tap);
            })
            .map_err(|e| -> PlatformError { Box::new(e) })?;

        match rx.recv() {
            Ok(Ok(_)) => Ok(()),
            _ => Err(Box::new(CaptureStartFailed)),
        }
    }

    fn stop(&self) -> PlatformResult<()> {
        let run_loop = self.inner.lock().expect("capture mutex").run_loop.take();
        if let Some(rl) = run_loop {
            rl.stop();
        }
        Ok(())
    }

    fn can_suppress(&self) -> bool {
        self.inner.lock().expect("capture mutex").can_suppress
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
