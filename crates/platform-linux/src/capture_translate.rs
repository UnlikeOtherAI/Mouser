//! Pure capture-direction event translation for [`crate::capture::LinuxCapture`].
//!
//! This is the host-shaped, side-effect-free half of Linux capture: a raw evdev
//! `(kind, code, value)` triple → the wire's [`LocalInputEvent`] model (HID
//! usages on Usage Page 0x07, §7.5 button indices, display-local cursor position).
//! Keeping it separate from the device/thread runtime in [`crate::capture`] keeps
//! both modules focused and lets the translation be unit-tested in isolation.
//!
//! Linux-only: it depends on `input_linux`'s `EventKind`/`KeyState` codes.

use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::{Mutex, MutexGuard, PoisonError};

use input_linux::{EventKind, KeyState};
use mouser_core::platform::LocalInputEvent;

use crate::display::{global_point_to_event, DisplayBounds, X11Display};
use crate::keymap::{
    evdev_btn_to_button, evdev_code_to_hid_usage, REL_HWHEEL, REL_WHEEL, REL_X, REL_Y,
};

/// Cursor position mapper for relative evdev motion.
///
/// Passive edge sensing uses XQueryPointer as the source of truth, then maps the
/// root-window point to a RandR output and display-local coordinates. Once the
/// engine asks to suppress local input, Xorg may stop receiving the grabbed evdev
/// device, so the mapper integrates deltas from the last real point until capture
/// returns to pass-through.
pub(crate) struct CursorMapper {
    display: Mutex<Option<X11Display>>,
    x: AtomicI32,
    y: AtomicI32,
    initialized: AtomicBool,
    integrating: AtomicBool,
    #[cfg(test)]
    test_displays: Option<Vec<DisplayBounds>>,
}

impl CursorMapper {
    pub(crate) fn new() -> Self {
        Self {
            display: Mutex::new(None),
            x: AtomicI32::new(0),
            y: AtomicI32::new(0),
            initialized: AtomicBool::new(false),
            integrating: AtomicBool::new(false),
            #[cfg(test)]
            test_displays: None,
        }
    }

    #[cfg(test)]
    pub(crate) fn with_static_display(display: DisplayBounds, x: i32, y: i32) -> Self {
        Self {
            display: Mutex::new(None),
            x: AtomicI32::new(x),
            y: AtomicI32::new(y),
            initialized: AtomicBool::new(true),
            integrating: AtomicBool::new(true),
            test_displays: Some(vec![display]),
        }
    }

    pub(crate) fn set_integrating(&self, integrating: bool) {
        self.integrating.store(integrating, Ordering::Relaxed);
    }

    fn store(&self, x: i32, y: i32) {
        self.x.store(x, Ordering::Relaxed);
        self.y.store(y, Ordering::Relaxed);
        self.initialized.store(true, Ordering::Relaxed);
    }

    fn apply(&self, dx: i32, dy: i32) -> Option<(i32, i32)> {
        if !self.initialized.load(Ordering::Relaxed) {
            return None;
        }
        let nx = self.x.load(Ordering::Relaxed).saturating_add(dx);
        let ny = self.y.load(Ordering::Relaxed).saturating_add(dy);
        self.x.store(nx, Ordering::Relaxed);
        self.y.store(ny, Ordering::Relaxed);
        Some((nx, ny))
    }

    fn displays(&self) -> Option<Vec<DisplayBounds>> {
        #[cfg(test)]
        if let Some(displays) = &self.test_displays {
            return Some(displays.clone());
        }

        let mut guard = lock_recover(&self.display);
        if guard.is_none() {
            *guard = X11Display::connect().ok();
        }
        let display = guard.as_ref()?;
        let displays_result = display.active_display_bounds();
        if displays_result.is_err() {
            *guard = None;
        }
        displays_result.ok()
    }

    fn real_position(&self) -> Option<(i32, i32, Vec<DisplayBounds>)> {
        let mut guard = lock_recover(&self.display);
        if guard.is_none() {
            *guard = X11Display::connect().ok();
        }
        let display = guard.as_ref()?;
        let position = display.cursor_global_position();
        let displays = display.active_display_bounds();
        if position.is_err() || displays.is_err() {
            *guard = None;
            return None;
        }
        let (x, y) = position.ok()?;
        Some((x, y, displays.ok()?))
    }

    fn event_from_delta(&self, dx: i32, dy: i32) -> Option<LocalInputEvent> {
        if !self.integrating.load(Ordering::Relaxed) {
            let (x, y, displays) = self.real_position()?;
            self.store(x, y);
            return Some(global_point_to_event(&displays, x, y));
        }
        let (x, y) = self.apply(dx, dy)?;
        let displays = self.displays()?;
        Some(global_point_to_event(&displays, x, y))
    }
}

fn lock_recover<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Translate one raw evdev event into a [`LocalInputEvent`], or `None` for events
/// we don't forward (SYN, key autorepeat, unmapped keys/axes, hi-res wheels).
///
/// `cursor` resolves relative pointer motion to display-local absolute motion.
pub(crate) fn to_local_event(
    kind: EventKind,
    code: u16,
    value: i32,
    cursor: &CursorMapper,
) -> Option<LocalInputEvent> {
    match kind {
        EventKind::Key => {
            // value: 0 = up, 1 = down, 2 = autorepeat (don't forward repeats).
            let down = match value {
                v if v == KeyState::PRESSED.value => true,
                v if v == KeyState::RELEASED.value => false,
                _ => return None, // autorepeat or unknown
            };
            // EV_KEY carries both keyboard keys and pointer buttons; split on code.
            if let Some(button) = evdev_btn_to_button(code) {
                return Some(LocalInputEvent::Button { button, down });
            }
            let usage = evdev_code_to_hid_usage(code)?;
            Some(LocalInputEvent::Key {
                usage,
                down,
                // Capture reports mods as 0; the engine derives modifier state
                // from the observed modifier-key transitions (HID 0xE0..=0xE7)
                // it also receives — same contract as platform-mac capture.
                mods: 0,
            })
        }
        EventKind::Relative => match code {
            REL_X => cursor.event_from_delta(value, 0),
            REL_Y => cursor.event_from_delta(0, value),
            // evdev wheel detents arrive as ±1; report in 1/120 units so the
            // engine's `Detent120` path sees whole notches (mirror of the
            // injector's `/120`).
            REL_WHEEL => Some(LocalInputEvent::Scroll {
                dx: 0,
                dy: value.saturating_mul(120),
            }),
            REL_HWHEEL => Some(LocalInputEvent::Scroll {
                dx: value.saturating_mul(120),
                dy: 0,
            }),
            _ => None, // hi-res wheels / other axes not forwarded
        },
        _ => None, // SYN / MSC / LED / etc.
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `to_local_event` is pure translation, exercised without any real device.
    fn cursor() -> CursorMapper {
        CursorMapper::new()
    }

    #[test]
    fn key_down_and_up_map_to_hid_usage() {
        let c = cursor();
        // KEY_A (code 30) press -> HID 0x04 down.
        assert_eq!(
            to_local_event(EventKind::Key, 30, 1, &c),
            Some(LocalInputEvent::Key {
                usage: 0x04,
                down: true,
                mods: 0
            })
        );
        // Release.
        assert_eq!(
            to_local_event(EventKind::Key, 30, 0, &c),
            Some(LocalInputEvent::Key {
                usage: 0x04,
                down: false,
                mods: 0
            })
        );
    }

    #[test]
    fn key_autorepeat_is_dropped() {
        let c = cursor();
        assert_eq!(to_local_event(EventKind::Key, 30, 2, &c), None);
    }

    #[test]
    fn buttons_map_to_wire_indices() {
        let c = cursor();
        // BTN_LEFT (0x110) down -> button 0.
        assert_eq!(
            to_local_event(EventKind::Key, 0x110, 1, &c),
            Some(LocalInputEvent::Button {
                button: 0,
                down: true
            })
        );
        // BTN_EXTRA (0x114) up -> button 4 (forward).
        assert_eq!(
            to_local_event(EventKind::Key, 0x114, 0, &c),
            Some(LocalInputEvent::Button {
                button: 4,
                down: false
            })
        );
    }

    #[test]
    fn suppressing_relative_motion_integrates_from_real_position() {
        let display = DisplayBounds {
            id: 7,
            left: 1000,
            top: 500,
            width: 300,
            height: 200,
        };
        let c = CursorMapper::with_static_display(display, 1050, 550);

        assert_eq!(
            to_local_event(EventKind::Relative, REL_X, 20, &c),
            Some(LocalInputEvent::CursorMoved {
                display_id: 7,
                x: 70,
                y: 50
            })
        );
        assert_eq!(
            to_local_event(EventKind::Relative, REL_Y, 10, &c),
            Some(LocalInputEvent::CursorMoved {
                display_id: 7,
                x: 70,
                y: 60
            })
        );
    }

    #[test]
    fn wheel_reports_detents_in_120_units() {
        let c = cursor();
        assert_eq!(
            to_local_event(EventKind::Relative, REL_WHEEL, 1, &c),
            Some(LocalInputEvent::Scroll { dx: 0, dy: 120 })
        );
        assert_eq!(
            to_local_event(EventKind::Relative, REL_HWHEEL, -1, &c),
            Some(LocalInputEvent::Scroll { dx: -120, dy: 0 })
        );
    }

    #[test]
    fn syn_and_unmapped_are_ignored() {
        let c = cursor();
        assert_eq!(to_local_event(EventKind::Synchronize, 0, 0, &c), None);
        // Unmapped key code (e.g. KEY_RESERVED 0).
        assert_eq!(to_local_event(EventKind::Key, 0, 1, &c), None);
        // Hi-res wheel axis not forwarded.
        assert_eq!(to_local_event(EventKind::Relative, 0x0b, 30, &c), None);
    }
}
