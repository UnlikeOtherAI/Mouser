//! Capture-direction key mapping: macOS `CGKeyCode` / `CGEventFlags` → the wire's
//! USB HID usages (Usage Page 0x07).
//!
//! The injection direction (HID usage → `CGKeyCode`, plus the `mods` bitmask
//! translation) lives in [`crate::keymap`]; this is its reverse, used only on the
//! capture path ([`crate::adapter::MacCapture`]). It also resolves macOS
//! `FlagsChanged` events — how modifier (Ctrl/Shift/Alt/Cmd) presses arrive on
//! macOS — into `(HID usage, down)` transitions the engine can forward.

use core_graphics::event::{CGEventFlags, CGKeyCode};

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

/// Resolve a macOS `FlagsChanged` event into the `(HID usage, down)` of the
/// physical modifier key that transitioned, or `None` when it can't be
/// determined.
///
/// `keycode` is the event's `CGKeyCode` (which physical modifier changed),
/// `prev_flags` the last-seen `CGEventFlags`, and `next_flags` the event's
/// current flags. A modifier is **down** when its flag bit is now SET but was
/// clear in `prev_flags` (toggled on) and **up** when it is now clear but was set
/// (toggled off).
///
/// Returns `None` when:
/// - the keycode is not a momentary modifier (Fn / `CapsLock`; see
///   [`cgkeycode_to_modifier_flag`]) or has no HID usage, or
/// - the key's flag bit did **not** change between `prev_flags` and `next_flags`.
///   With left/right twins sharing one flag, releasing one while the other is
///   still held leaves the net flag set, so that release is indistinguishable and
///   is dropped rather than reported as a spurious down — a documented macOS
///   shared-flag limitation.
#[must_use]
pub fn flags_changed_event(
    keycode: CGKeyCode,
    prev_flags: CGEventFlags,
    next_flags: CGEventFlags,
) -> Option<(u16, bool)> {
    let bit = cgkeycode_to_modifier_flag(keycode)?;
    let usage = cgkeycode_to_hid_usage(keycode)?;
    let was_set = prev_flags.contains(bit);
    let now_set = next_flags.contains(bit);
    match (was_set, now_set) {
        (false, true) => Some((usage, true)),  // toggled on  -> down
        (true, false) => Some((usage, false)), // toggled off -> up
        _ => None,                             // no net change -> can't tell
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
    fn flags_changed_down_when_bit_set() {
        // Left Command (0x37) pressed: Command flag goes clear -> set => down,
        // HID usage 0xE3 (Left GUI/Command).
        assert_eq!(
            flags_changed_event(
                0x37,
                CGEventFlags::CGEventFlagNull,
                CGEventFlags::CGEventFlagCommand
            ),
            Some((0xE3, true))
        );
        // Left Control (0x3B) pressed -> usage 0xE0.
        assert_eq!(
            flags_changed_event(
                0x3B,
                CGEventFlags::CGEventFlagNull,
                CGEventFlags::CGEventFlagControl
            ),
            Some((0xE0, true))
        );
    }

    #[test]
    fn flags_changed_up_when_bit_cleared() {
        // Left Shift (0x38) released: Shift flag set -> clear => up, usage 0xE1.
        assert_eq!(
            flags_changed_event(
                0x38,
                CGEventFlags::CGEventFlagShift,
                CGEventFlags::CGEventFlagNull
            ),
            Some((0xE1, false))
        );
        // Right Option (0x3D) released -> usage 0xE6.
        assert_eq!(
            flags_changed_event(
                0x3D,
                CGEventFlags::CGEventFlagAlternate,
                CGEventFlags::CGEventFlagNull
            ),
            Some((0xE6, false))
        );
    }

    #[test]
    fn flags_changed_right_twins_use_their_own_usage() {
        // Right Shift (0x3C) and Right Command (0x36) share their family's flag
        // but map to the right-hand HID usages (0xE5, 0xE7).
        assert_eq!(
            flags_changed_event(
                0x3C,
                CGEventFlags::CGEventFlagNull,
                CGEventFlags::CGEventFlagShift
            ),
            Some((0xE5, true))
        );
        assert_eq!(
            flags_changed_event(
                0x36,
                CGEventFlags::CGEventFlagNull,
                CGEventFlags::CGEventFlagCommand
            ),
            Some((0xE7, true))
        );
    }

    #[test]
    fn flags_changed_no_change_is_none() {
        // Shared-flag limitation: releasing Left Shift while Right Shift is still
        // held leaves the Shift flag set (no net change) -> not reported.
        assert_eq!(
            flags_changed_event(
                0x38,
                CGEventFlags::CGEventFlagShift,
                CGEventFlags::CGEventFlagShift
            ),
            None
        );
        // Bit unrelated to this key changed (Command toggled, key is Shift):
        // Shift bit unchanged -> None.
        assert_eq!(
            flags_changed_event(
                0x38,
                CGEventFlags::CGEventFlagNull,
                CGEventFlags::CGEventFlagCommand
            ),
            None
        );
    }

    #[test]
    fn flags_changed_caps_and_fn_out_of_scope() {
        // CapsLock (0x39) is a lock toggle, Fn (0x3F) carries no HID usage here;
        // both are out of scope and never produce a Key event.
        assert_eq!(
            flags_changed_event(
                0x39,
                CGEventFlags::CGEventFlagNull,
                CGEventFlags::CGEventFlagAlphaShift
            ),
            None
        );
        assert_eq!(
            flags_changed_event(
                0x3F,
                CGEventFlags::CGEventFlagNull,
                CGEventFlags::CGEventFlagSecondaryFn
            ),
            None
        );
    }
}
