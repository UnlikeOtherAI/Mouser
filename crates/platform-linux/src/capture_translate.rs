//! Pure capture-direction event translation for [`crate::capture::LinuxCapture`].
//!
//! This is the host-shaped, side-effect-free half of Linux capture: a raw evdev
//! `(kind, code, value)` triple → the wire's [`LocalInputEvent`] model (HID
//! usages on Usage Page 0x07, §7.5 button indices, integrated cursor position).
//! Keeping it separate from the device/thread runtime in [`crate::capture`] keeps
//! both modules focused and lets the translation be unit-tested in isolation.
//!
//! Linux-only: it depends on `input_linux`'s `EventKind`/`KeyState` codes.

use std::sync::atomic::{AtomicI32, Ordering};

use input_linux::{EventKind, KeyState};
use mouser_core::platform::LocalInputEvent;

use crate::keymap::{
    evdev_btn_to_button, evdev_code_to_hid_usage, REL_HWHEEL, REL_WHEEL, REL_X, REL_Y,
};

/// Integrated virtual-cursor position for relative-motion → absolute translation.
///
/// evdev pointers report relative deltas (`REL_X`/`REL_Y`); the wire wants an
/// absolute [`LocalInputEvent::CursorMoved`]. This accumulates the deltas and
/// clamps to a desktop bound (documented single-global-space limitation — see
/// [`crate::capture`] module docs).
pub(crate) struct VirtualCursor {
    x: AtomicI32,
    y: AtomicI32,
    w: i32,
    h: i32,
}

impl VirtualCursor {
    /// Start centered in a `w × h` logical-pixel desktop bound.
    pub(crate) fn new(w: i32, h: i32) -> Self {
        Self {
            x: AtomicI32::new(w / 2),
            y: AtomicI32::new(h / 2),
            w,
            h,
        }
    }

    /// Apply a relative delta, clamp to the desktop bound, and return the new
    /// absolute position.
    fn apply(&self, dx: i32, dy: i32) -> (i32, i32) {
        let nx = (self.x.load(Ordering::Relaxed).saturating_add(dx)).clamp(0, self.w);
        let ny = (self.y.load(Ordering::Relaxed).saturating_add(dy)).clamp(0, self.h);
        self.x.store(nx, Ordering::Relaxed);
        self.y.store(ny, Ordering::Relaxed);
        (nx, ny)
    }
}

/// Translate one raw evdev event into a [`LocalInputEvent`], or `None` for events
/// we don't forward (SYN, key autorepeat, unmapped keys/axes, hi-res wheels).
///
/// `cursor` integrates relative pointer motion into an absolute position
/// (documented single-global-space limitation).
pub(crate) fn to_local_event(
    kind: EventKind,
    code: u16,
    value: i32,
    cursor: &VirtualCursor,
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
            REL_X => {
                let (x, y) = cursor.apply(value, 0);
                Some(LocalInputEvent::CursorMoved {
                    display_id: 0,
                    x,
                    y,
                })
            }
            REL_Y => {
                let (x, y) = cursor.apply(0, value);
                Some(LocalInputEvent::CursorMoved {
                    display_id: 0,
                    x,
                    y,
                })
            }
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
    fn cursor() -> VirtualCursor {
        VirtualCursor::new(1000, 1000)
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
    fn relative_motion_integrates_and_clamps() {
        let c = VirtualCursor::new(100, 100); // starts centered at (50, 50)
        // Move right by 20.
        assert_eq!(
            to_local_event(EventKind::Relative, REL_X, 20, &c),
            Some(LocalInputEvent::CursorMoved {
                display_id: 0,
                x: 70,
                y: 50
            })
        );
        // Move down by 70 -> clamped to the bound (100).
        assert_eq!(
            to_local_event(EventKind::Relative, REL_Y, 70, &c),
            Some(LocalInputEvent::CursorMoved {
                display_id: 0,
                x: 70,
                y: 100
            })
        );
        // Move far left -> clamped to 0.
        assert_eq!(
            to_local_event(EventKind::Relative, REL_X, -500, &c),
            Some(LocalInputEvent::CursorMoved {
                display_id: 0,
                x: 0,
                y: 100
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
