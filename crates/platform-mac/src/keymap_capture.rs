//! Capture-direction event mapping: a captured macOS `CGEvent` → the wire's
//! [`LocalInputEvent`] model (HID usages on Usage Page 0x07, per-display motion).
//!
//! The injection direction (HID usage → `CGKeyCode`, plus the `mods` bitmask
//! translation) lives in [`crate::keymap`]; this is its reverse, used only on the
//! capture path ([`crate::adapter::MacCapture`]). It:
//! - reverses keycodes ([`cgkeycode_to_hid_usage`]),
//! - resolves macOS `FlagsChanged` modifier transitions
//!   ([`flags_changed_event`] / [`ModifierState`]),
//! - and translates a whole captured `CGEvent` into a [`LocalInputEvent`]
//!   ([`to_local_event`]), resolving cursor motion to its containing display
//!   (audit C2-9).

use core_graphics::event::{CGEvent, CGEventFlags, CGEventType, CGKeyCode, EventField};
use mouser_core::platform::LocalInputEvent;

use crate::display_info::{active_display_bounds, display_for_global_point};
use crate::keymap::hid_usage_to_cgkeycode;

/// Reverse of [`crate::keymap::hid_usage_to_cgkeycode`]: a macOS `CGKeyCode` → its
/// HID usage (Usage Page 0x07), or `None` if no mapped usage produces that
/// keycode.
///
/// Used by capture to report locally-observed keys as HID usages
/// ([`crate::adapter::MacCapture`]). Linear over the mapped set — small and only
/// on the (cold) capture path.
#[must_use]
pub fn cgkeycode_to_hid_usage(keycode: CGKeyCode) -> Option<u16> {
    (0x04u16..=0xE7).find(|&u| hid_usage_to_cgkeycode(u) == Some(keycode))
}

/// The `CGEventFlags` bit a *momentary modifier* `CGKeyCode` toggles, or `None`
/// for keycodes that are not momentary modifiers.
///
/// macOS delivers modifier transitions as `FlagsChanged` events (not KeyDown/
/// KeyUp); the changed key is identified by its `CGKeyCode` and its effect is a
/// single device-independent flag bit. Left/right twins of a family fold to the
/// **same** flag (macOS flags carry no handedness), e.g. both `kVK_Shift` (0x38)
/// and `kVK_RightShift` (0x3C) → [`CGEventFlags::CGEventFlagShift`].
///
/// **Out of scope** (returns `None`): `kVK_CapsLock` (0x39 →
/// `CGEventFlagAlphaShift`) is a *lock toggle*, not a momentary press/release, so
/// treating it as a held modifier would be wrong; `kVK_Function` (0x3F →
/// `CGEventFlagSecondaryFn`, the Fn key) carries no HID Usage-Page-0x07 usage in
/// this table. Neither is forwarded as a [`crate::adapter`] `Key` event.
#[must_use]
fn cgkeycode_to_modifier_flag(keycode: CGKeyCode) -> Option<CGEventFlags> {
    match keycode {
        0x3B | 0x3E => Some(CGEventFlags::CGEventFlagControl), // L/R Control
        0x38 | 0x3C => Some(CGEventFlags::CGEventFlagShift),   // L/R Shift
        0x3A | 0x3D => Some(CGEventFlags::CGEventFlagAlternate), // L/R Option
        0x37 | 0x36 => Some(CGEventFlags::CGEventFlagCommand), // L/R Command
        _ => None,
    }
}

/// Per-capture record of which momentary-modifier `CGKeyCode`s are currently held.
///
/// macOS delivers each modifier press/release as a `FlagsChanged` carrying the
/// physical key's `CGKeyCode`; **each event for a given keyCode toggles that
/// key's state**. Tracking per-keyCode held state (rather than only the shared
/// device-independent flag bit) is what lets us report the release of one of two
/// held twins (e.g. left Shift while right Shift stays down) — the shared flag
/// stays set across that release, so a flag-bit diff alone would drop it and
/// leave a stuck modifier on the remote (audit FlagsChanged-twin).
///
/// Modifiers are a tiny fixed set (≤8 keycodes), so a `Vec` used as a small set
/// is simpler than a bitset and keeps the (cold) capture path allocation-light.
#[derive(Debug, Default, Clone)]
pub struct ModifierState {
    held: Vec<CGKeyCode>,
}

impl ModifierState {
    /// A state with no modifiers held.
    #[must_use]
    pub fn new() -> Self {
        Self { held: Vec::new() }
    }

    /// Toggle the held state of `keycode` and report the resulting transition:
    /// `true` if it is now **down** (was up), `false` if now **up** (was down).
    fn toggle(&mut self, keycode: CGKeyCode) -> bool {
        if let Some(pos) = self.held.iter().position(|&k| k == keycode) {
            self.held.swap_remove(pos);
            false // was held -> released
        } else {
            self.held.push(keycode);
            true // was not held -> pressed
        }
    }

    /// HID modifier bitmask for the modifier keys currently held.
    #[must_use]
    pub fn modifier_bits(&self) -> u16 {
        let mut bits = 0;
        for keycode in &self.held {
            if let Some(usage) = cgkeycode_to_hid_usage(*keycode).and_then(modifier_bit) {
                bits |= usage;
            }
        }
        bits
    }
}

fn modifier_bit(usage: u16) -> Option<u16> {
    if (0xE0..=0xE7).contains(&usage) {
        Some(1 << (usage - 0xE0))
    } else {
        None
    }
}

fn saturating_i64_to_i32(value: i64) -> i32 {
    i32::try_from(value).unwrap_or(if value < 0 { i32::MIN } else { i32::MAX })
}

/// Resolve a macOS `FlagsChanged` event into the `(HID usage, down)` of the
/// physical modifier key that transitioned, or `None` when the keycode is not a
/// forwardable momentary modifier.
///
/// `keycode` is the event's `CGKeyCode` (which physical modifier changed) and
/// `state` is the per-capture [`ModifierState`] tracking currently-held modifier
/// keycodes, which this call **updates**. Direction is derived from that
/// per-keyCode state — each `FlagsChanged` for a keyCode toggles it — so the
/// release of one of two held left/right twins is reported correctly even though
/// they share a single device-independent flag bit (audit FlagsChanged-twin).
///
/// Returns `None` only when the keycode is not a momentary modifier (Fn /
/// `CapsLock`; see [`cgkeycode_to_modifier_flag`]) or has no HID usage — those
/// are never forwarded as `Key` events and so don't mutate `state`.
#[must_use]
pub fn flags_changed_event(keycode: CGKeyCode, state: &mut ModifierState) -> Option<(u16, bool)> {
    // Only momentary modifiers with a HID usage are forwarded; CapsLock/Fn and
    // anything unmapped are ignored without touching the held-set.
    cgkeycode_to_modifier_flag(keycode)?;
    let usage = cgkeycode_to_hid_usage(keycode)?;
    let down = state.toggle(keycode);
    Some((usage, down))
}

/// Resolve a **global** CG cursor point to a [`LocalInputEvent::CursorMoved`] in
/// the wire's per-display space (audit C2-9): the `display_id` of the containing
/// display and display-LOCAL `(x, y)` (§7.6 / Appendix A), not `display_id:0` +
/// global coords.
///
/// Sub-pixel precision is preserved through the subtraction and only truncated to
/// the wire's `i32` at the end (`f64 as i32` saturates, never UB). A point that
/// lands in no active display (a momentary off-screen sample during display
/// hot-plug, or the far seam) falls back to the **main** display so motion still
/// resolves to a real monitor instead of a bogus `display_id:0`.
#[must_use]
pub fn cursor_moved_for_global(gx: f64, gy: f64, dx: i32, dy: i32) -> LocalInputEvent {
    let bounds =
        display_for_global_point(gx, gy).or_else(|| active_display_bounds().into_iter().next());
    match bounds {
        Some(b) => {
            let (lx, ly) = b.global_to_local(gx, gy);
            LocalInputEvent::CursorMoved {
                display_id: b.id,
                x: lx as i32,
                y: ly as i32,
                dx,
                dy,
            }
        }
        // No active displays at all (headless): report the raw point on id 0.
        None => LocalInputEvent::CursorMoved {
            display_id: 0,
            x: gx as i32,
            y: gy as i32,
            dx,
            dy,
        },
    }
}

/// Translate a captured `CGEvent` into a [`LocalInputEvent`], or `None` for event
/// types we don't forward.
///
/// Captured `mods` is reported as `0`: the engine derives modifier state from the
/// observed modifier **key** transitions (HID 0xE0..=0xE7) it also receives.
/// `FlagsChanged` is **not** handled here — modifier transitions need the
/// per-keyCode held state ([`ModifierState`]), so they are resolved in the tap
/// callback via [`flags_changed_event`] (see [`crate::adapter`]).
#[must_use]
pub fn to_local_event(etype: CGEventType, event: &CGEvent) -> Option<LocalInputEvent> {
    // Drop our own injected events: they carry SYNTHETIC_EVENT_TAG in kCGEventSourceUserData.
    // Modelling them as "not local input" (None) means they are never forwarded as a bogus
    // peer delta nor mistaken for the user grabbing the mouse back, while the tap callback
    // still passes them through so they take effect. Makes the inject→capture recapture loop
    // impossible by construction, independent of any "the source only ever warps" discipline.
    if event.get_integer_value_field(EventField::EVENT_SOURCE_USER_DATA)
        == crate::inject::SYNTHETIC_EVENT_TAG
    {
        return None;
    }
    match etype {
        CGEventType::MouseMoved
        | CGEventType::LeftMouseDragged
        | CGEventType::RightMouseDragged
        | CGEventType::OtherMouseDragged => {
            let p = event.location();
            // Relative device deltas (logical px) — valid even when the cursor is parked at
            // a screen edge or suppressed, so a controlled peer's cursor can traverse fully.
            let dx = event.get_integer_value_field(EventField::MOUSE_EVENT_DELTA_X) as i32;
            let dy = event.get_integer_value_field(EventField::MOUSE_EVENT_DELTA_Y) as i32;
            Some(cursor_moved_for_global(p.x, p.y, dx, dy))
        }
        CGEventType::LeftMouseDown => Some(LocalInputEvent::Button {
            button: 0,
            down: true,
        }),
        CGEventType::LeftMouseUp => Some(LocalInputEvent::Button {
            button: 0,
            down: false,
        }),
        CGEventType::RightMouseDown => Some(LocalInputEvent::Button {
            button: 1,
            down: true,
        }),
        CGEventType::RightMouseUp => Some(LocalInputEvent::Button {
            button: 1,
            down: false,
        }),
        CGEventType::OtherMouseDown | CGEventType::OtherMouseUp => {
            let n = event.get_integer_value_field(EventField::MOUSE_EVENT_BUTTON_NUMBER);
            let down = matches!(etype, CGEventType::OtherMouseDown);
            u8::try_from(n)
                .ok()
                .map(|button| LocalInputEvent::Button { button, down })
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
                dx: saturating_i64_to_i32(dx),
                dy: saturating_i64_to_i32(dy),
            })
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reverse_map_round_trips_known_keys() {
        // Left Command keycode 0x37 -> HID 0xE3; Escape keycode 0x35 -> HID 0x29.
        assert_eq!(cgkeycode_to_hid_usage(0x37), Some(0xE3));
        assert_eq!(cgkeycode_to_hid_usage(0x35), Some(0x29));
        // An unmapped keycode has no usage.
        assert_eq!(cgkeycode_to_hid_usage(0xFF), None);
    }

    #[test]
    fn flags_changed_down_then_up_for_one_key() {
        let mut s = ModifierState::new();
        // Left Command (0x37) first FlagsChanged -> down, HID usage 0xE3.
        assert_eq!(flags_changed_event(0x37, &mut s), Some((0xE3, true)));
        // Next FlagsChanged for the same key -> up.
        assert_eq!(flags_changed_event(0x37, &mut s), Some((0xE3, false)));
        // Left Control (0x3B) -> usage 0xE0, down.
        assert_eq!(flags_changed_event(0x3B, &mut s), Some((0xE0, true)));
    }

    #[test]
    fn flags_changed_right_twins_use_their_own_usage() {
        let mut s = ModifierState::new();
        // Right Shift (0x3C) and Right Command (0x36) map to the right-hand HID
        // usages (0xE5, 0xE7) on press.
        assert_eq!(flags_changed_event(0x3C, &mut s), Some((0xE5, true)));
        assert_eq!(flags_changed_event(0x36, &mut s), Some((0xE7, true)));
    }

    #[test]
    fn modifier_bits_include_held_left_and_right_ctrl() {
        let mut s = ModifierState::new();
        assert_eq!(flags_changed_event(0x3B, &mut s), Some((0xE0, true)));
        assert_eq!(flags_changed_event(0x3E, &mut s), Some((0xE4, true)));
        assert_eq!(s.modifier_bits(), (1 << 0) | (1 << 4));
    }

    #[test]
    fn scroll_delta_conversion_saturates_instead_of_wrapping() {
        assert_eq!(saturating_i64_to_i32(i64::from(i32::MAX) + 1), i32::MAX);
        assert_eq!(saturating_i64_to_i32(i64::from(i32::MIN) - 1), i32::MIN);
        assert_eq!(saturating_i64_to_i32(-120), -120);
    }

    #[test]
    fn flags_changed_releases_one_twin_while_the_other_is_held() {
        // The audit FlagsChanged-twin case: hold BOTH shift keys, then release
        // one. The shared Shift flag stays set the whole time, but each release
        // must still be reported (keyed on the keyCode) so no modifier sticks on
        // the remote.
        let mut s = ModifierState::new();
        assert_eq!(flags_changed_event(0x38, &mut s), Some((0xE1, true))); // L Shift down
        assert_eq!(flags_changed_event(0x3C, &mut s), Some((0xE5, true))); // R Shift down
                                                                           // Release left shift: previously dropped (no net flag change); now a
                                                                           // proper up for the LEFT usage 0xE1.
        assert_eq!(flags_changed_event(0x38, &mut s), Some((0xE1, false)));
        // Right shift still tracked as held -> its release is also reported.
        assert_eq!(flags_changed_event(0x3C, &mut s), Some((0xE5, false)));
    }

    #[test]
    fn flags_changed_caps_and_fn_out_of_scope() {
        // CapsLock (0x39) is a lock toggle, Fn (0x3F) carries no HID usage here;
        // both are out of scope, never produce a Key event, and do not pollute
        // the held-set.
        let mut s = ModifierState::new();
        assert_eq!(flags_changed_event(0x39, &mut s), None);
        assert_eq!(flags_changed_event(0x3F, &mut s), None);
        // An unrelated real modifier still toggles cleanly afterward.
        assert_eq!(flags_changed_event(0x38, &mut s), Some((0xE1, true)));
    }

    #[test]
    fn captured_cursor_resolves_to_a_real_display_and_local_coords() {
        // Audit C2-9: a captured global point must report the containing
        // display's real id and display-LOCAL coords — never `display_id:0` +
        // global coords. Use the main display (always present on a CI mac host)
        // and a point known to sit inside it.
        let main = crate::display_info::main_display_bounds();
        // A point one quarter into the display, in GLOBAL coordinates.
        let gx = main.x + main.w / 4.0;
        let gy = main.y + main.h / 4.0;
        let ev = cursor_moved_for_global(gx, gy, 0, 0);
        let LocalInputEvent::CursorMoved {
            display_id, x, y, ..
        } = ev
        else {
            panic!("expected CursorMoved, got {ev:?}");
        };
        // Resolves to the main display (containing point), not a bogus 0...
        assert_eq!(display_id, main.id);
        // ...with coordinates expressed display-LOCAL (origin subtracted).
        assert_eq!(x, (gx - main.x) as i32);
        assert_eq!(y, (gy - main.y) as i32);
        // The local coords differ from the raw global ones whenever the display
        // origin is non-zero (multi-monitor); on a single primary at (0,0) they
        // coincide, which is still correct.
        if main.x != 0.0 || main.y != 0.0 {
            assert!(x != gx as i32 || y != gy as i32);
        }
    }
}
