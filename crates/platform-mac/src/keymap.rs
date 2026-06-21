//! HID Usage (USB HID Usage Page 0x07) → macOS virtual key code (`CGKeyCode`).
//!
//! The Mouser wire protocol carries keys as **USB HID Usage IDs on Usage Page
//! 0x07** (see `docs/communication-interface.md` §7.5 + Appendix B). macOS
//! injection (`CGEventCreateKeyboardEvent`) wants a *virtual key code*
//! (`CGKeyCode`, the Carbon/`Events.h` codes). This module is the macOS half of
//! the bidirectional table Appendix B mandates each platform adapter ship.
//!
//! Coverage (audit H11): letters, number row, whitespace/control, punctuation,
//! arrows, modifiers, **F1–F12**, the **nav cluster** (Insert/Home/PageUp/
//! ForwardDelete/End/PageDown), and the **keypad**. The exact HID-usage set this
//! table covers is mirrored by `platform-linux`'s evdev table and asserted equal
//! by a parity test ([`supported_hid_usages`]).
//!
//! It also translates the wire `mods` bitmask (Appendix B) into the modifier HID
//! usages / `CGKeyCode`s so an adapter can press/release modifiers around a key.

use core_graphics::event::{CGEventFlags, CGKeyCode};

/// Translate a USB HID Usage (Usage Page 0x07) to a macOS `CGKeyCode`.
///
/// Returns `None` for usages not in the table.
#[must_use]
pub fn hid_usage_to_cgkeycode(usage: u16) -> Option<CGKeyCode> {
    // Source of truth for CGKeyCode values: Carbon `Events.h` (`kVK_*`).
    // HID usage values: USB HID Usage Tables, Page 0x07.
    let code: CGKeyCode = match usage {
        // Letters a..z (HID 0x04..=0x1D)
        0x04 => 0x00, // a
        0x05 => 0x0B, // b
        0x06 => 0x08, // c
        0x07 => 0x02, // d
        0x08 => 0x0E, // e
        0x09 => 0x03, // f
        0x0A => 0x05, // g
        0x0B => 0x04, // h
        0x0C => 0x22, // i
        0x0D => 0x26, // j
        0x0E => 0x28, // k
        0x0F => 0x25, // l
        0x10 => 0x2E, // m
        0x11 => 0x2D, // n
        0x12 => 0x1F, // o
        0x13 => 0x23, // p
        0x14 => 0x0C, // q
        0x15 => 0x0F, // r
        0x16 => 0x01, // s
        0x17 => 0x11, // t
        0x18 => 0x20, // u
        0x19 => 0x09, // v
        0x1A => 0x0D, // w
        0x1B => 0x07, // x
        0x1C => 0x10, // y
        0x1D => 0x06, // z

        // Number row 1..0 (HID 0x1E..=0x27)
        0x1E => 0x12, // 1
        0x1F => 0x13, // 2
        0x20 => 0x14, // 3
        0x21 => 0x15, // 4
        0x22 => 0x17, // 5
        0x23 => 0x16, // 6
        0x24 => 0x1A, // 7
        0x25 => 0x1C, // 8
        0x26 => 0x19, // 9
        0x27 => 0x1D, // 0

        // Whitespace / control
        0x28 => 0x24, // Enter / Return
        0x29 => 0x35, // Escape
        0x2A => 0x33, // Backspace (Delete)
        0x2B => 0x30, // Tab
        0x2C => 0x31, // Space

        // Punctuation
        0x2D => 0x1B, // - and _
        0x2E => 0x18, // = and +
        0x2F => 0x21, // [ and {
        0x30 => 0x1E, // ] and }
        0x31 => 0x2A, // \ and |
        0x33 => 0x29, // ; and :
        0x34 => 0x27, // ' and "
        0x35 => 0x32, // ` and ~
        0x36 => 0x2B, // , and <
        0x37 => 0x2F, // . and >
        0x38 => 0x2C, // / and ?
        0x39 => 0x39, // Caps Lock

        // Function row F1..F12 (HID 0x3A..=0x45)
        0x3A => 0x7A, // F1
        0x3B => 0x78, // F2
        0x3C => 0x63, // F3
        0x3D => 0x76, // F4
        0x3E => 0x60, // F5
        0x3F => 0x61, // F6
        0x40 => 0x62, // F7
        0x41 => 0x64, // F8
        0x42 => 0x65, // F9
        0x43 => 0x6D, // F10
        0x44 => 0x67, // F11
        0x45 => 0x6F, // F12

        // Navigation cluster (HID 0x49..=0x4E). macOS has no physical Insert;
        // the Apple "Help" key (kVK_Help) occupies the Insert position.
        0x49 => 0x72, // Insert  -> Help
        0x4A => 0x73, // Home
        0x4B => 0x74, // Page Up
        0x4C => 0x75, // Delete Forward (ForwardDelete)
        0x4D => 0x77, // End
        0x4E => 0x79, // Page Down

        // Arrows (HID 0x4F..=0x52)
        0x4F => 0x7C, // Right
        0x50 => 0x7B, // Left
        0x51 => 0x7D, // Down
        0x52 => 0x7E, // Up

        // Keypad (HID 0x53..=0x63, plus 0x67 KpEqual, 0x85 KpComma)
        0x53 => 0x47, // Num Lock / Keypad Clear (kVK_ANSI_KeypadClear)
        0x54 => 0x4B, // Keypad /
        0x55 => 0x43, // Keypad *
        0x56 => 0x4E, // Keypad -
        0x57 => 0x45, // Keypad +
        0x58 => 0x4C, // Keypad Enter
        0x59 => 0x53, // Keypad 1
        0x5A => 0x54, // Keypad 2
        0x5B => 0x55, // Keypad 3
        0x5C => 0x56, // Keypad 4
        0x5D => 0x57, // Keypad 5
        0x5E => 0x58, // Keypad 6
        0x5F => 0x59, // Keypad 7
        0x60 => 0x5B, // Keypad 8
        0x61 => 0x5C, // Keypad 9
        0x62 => 0x52, // Keypad 0
        0x63 => 0x41, // Keypad .
        0x67 => 0x51, // Keypad =
        0x85 => 0x5F, // Keypad , (kVK_JIS_KeypadComma)

        // Modifiers (HID 0xE0..=0xE7)
        0xE0 => 0x3B, // Left Ctrl
        0xE1 => 0x38, // Left Shift
        0xE2 => 0x3A, // Left Alt / Option
        0xE3 => 0x37, // Left GUI / Command
        0xE4 => 0x3E, // Right Ctrl
        0xE5 => 0x3C, // Right Shift
        0xE6 => 0x3D, // Right Alt / Option
        0xE7 => 0x36, // Right GUI / Command

        _ => return None,
    };
    Some(code)
}

/// One active modifier the wire `mods` bitmask can carry (Appendix B bit order).
///
/// Each entry is the HID usage of the modifier key; the adapter resolves it to a
/// `CGKeyCode` via [`hid_usage_to_cgkeycode`] after applying the optional
/// Cmd↔Ctrl swap.
const MOD_BITS: [(u16, u16); 8] = [
    (0, 0xE0), // bit0  Left Ctrl
    (1, 0xE1), // bit1  Left Shift
    (2, 0xE2), // bit2  Left Alt
    (3, 0xE3), // bit3  Left Meta (Command)
    (4, 0xE4), // bit4  Right Ctrl
    (5, 0xE5), // bit5  Right Shift
    (6, 0xE6), // bit6  Right Alt
    (7, 0xE7), // bit7  Right Meta (Command)
];

/// Apply the optional macOS Cmd↔Ctrl swap to a modifier HID usage.
///
/// When `cmd_ctrl_swap` is set (input pref, Appendix A `input_prefs`), a remote
/// machine's Ctrl is delivered as macOS Command and vice-versa, so muscle memory
/// from Windows/Linux maps to the mac shortcut layout. Swaps both left and right.
#[must_use]
fn swap_cmd_ctrl(usage: u16, cmd_ctrl_swap: bool) -> u16 {
    if !cmd_ctrl_swap {
        return usage;
    }
    match usage {
        0xE0 => 0xE3, // L Ctrl -> L Cmd
        0xE3 => 0xE0, // L Cmd  -> L Ctrl
        0xE4 => 0xE7, // R Ctrl -> R Cmd
        0xE7 => 0xE4, // R Cmd  -> R Ctrl
        other => other,
    }
}

/// Translate the wire `mods` bitmask (Appendix B) into the `CGKeyCode`s of the
/// modifier keys that are held down.
///
/// `Meta` (bits 3/7) is macOS **Command**. With `cmd_ctrl_swap`, Ctrl and
/// Command are exchanged (Appendix B "optional Cmd↔Ctrl swap"). Unknown bits are
/// ignored. The returned order follows the bit order so press/release is
/// deterministic.
#[must_use]
pub fn mods_to_cgkeycodes(mods: u16, cmd_ctrl_swap: bool) -> Vec<CGKeyCode> {
    let mut out = Vec::new();
    for (bit, usage) in MOD_BITS {
        if mods & (1 << bit) != 0 {
            let usage = swap_cmd_ctrl(usage, cmd_ctrl_swap);
            if let Some(code) = hid_usage_to_cgkeycode(usage) {
                out.push(code);
            }
        }
    }
    out
}

/// Translate the wire `mods` bitmask (Appendix B) into device-independent
/// `CGEventFlags` for a synthetic event.
///
/// This is the path the injector uses: setting the event's flags makes the
/// window server treat the synthesized key as part of a chord (e.g. ⌘C) without
/// also posting standalone modifier key events. macOS flags don't distinguish
/// left/right, so the left and right bits of each family fold to the same flag.
/// `Meta` → Command; with `cmd_ctrl_swap` the Ctrl and Command flags are
/// exchanged (Appendix B).
#[must_use]
pub fn mods_to_cgflags(mods: u16, cmd_ctrl_swap: bool) -> CGEventFlags {
    let ctrl = mods & ((1 << 0) | (1 << 4)) != 0;
    let shift = mods & ((1 << 1) | (1 << 5)) != 0;
    let alt = mods & ((1 << 2) | (1 << 6)) != 0;
    let meta = mods & ((1 << 3) | (1 << 7)) != 0;

    // Resolve which flag Ctrl vs Command each maps to under the optional swap.
    let (ctrl_flag, meta_flag) = if cmd_ctrl_swap {
        (
            CGEventFlags::CGEventFlagCommand,
            CGEventFlags::CGEventFlagControl,
        )
    } else {
        (
            CGEventFlags::CGEventFlagControl,
            CGEventFlags::CGEventFlagCommand,
        )
    };

    let mut flags = CGEventFlags::CGEventFlagNull;
    if ctrl {
        flags |= ctrl_flag;
    }
    if shift {
        flags |= CGEventFlags::CGEventFlagShift;
    }
    if alt {
        flags |= CGEventFlags::CGEventFlagAlternate;
    }
    if meta {
        flags |= meta_flag;
    }
    flags
}

/// The exact set of HID usages this macOS table maps, sorted ascending.
///
/// Mirrored by `platform_linux::keymap::supported_hid_usages`; a parity test
/// asserts the two are identical (no collisions, same coverage — audit H11).
#[must_use]
pub fn supported_hid_usages() -> Vec<u16> {
    let mut v: Vec<u16> = (0x04u16..=0xE7)
        .filter(|&u| hid_usage_to_cgkeycode(u).is_some())
        .collect();
    v.sort_unstable();
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_known_usages() {
        assert_eq!(hid_usage_to_cgkeycode(0x04), Some(0x00)); // a
        assert_eq!(hid_usage_to_cgkeycode(0x28), Some(0x24)); // Enter
        assert_eq!(hid_usage_to_cgkeycode(0x29), Some(0x35)); // Escape
        assert_eq!(hid_usage_to_cgkeycode(0x2C), Some(0x31)); // Space
        assert_eq!(hid_usage_to_cgkeycode(0xE3), Some(0x37)); // Left Cmd
    }

    #[test]
    fn maps_function_nav_keypad() {
        assert_eq!(hid_usage_to_cgkeycode(0x3A), Some(0x7A)); // F1
        assert_eq!(hid_usage_to_cgkeycode(0x45), Some(0x6F)); // F12
        assert_eq!(hid_usage_to_cgkeycode(0x4A), Some(0x73)); // Home
        assert_eq!(hid_usage_to_cgkeycode(0x4E), Some(0x79)); // Page Down
        assert_eq!(hid_usage_to_cgkeycode(0x58), Some(0x4C)); // Keypad Enter
        assert_eq!(hid_usage_to_cgkeycode(0x62), Some(0x52)); // Keypad 0
    }

    #[test]
    fn unknown_usage_is_none() {
        assert_eq!(hid_usage_to_cgkeycode(0xFFFF), None);
        assert_eq!(hid_usage_to_cgkeycode(0x00), None);
    }

    #[test]
    fn mods_translate_meta_to_command() {
        // bit3 = Left Meta -> Left Command (CGKeyCode 0x37), no swap.
        assert_eq!(mods_to_cgkeycodes(1 << 3, false), vec![0x37]);
        // bit0 = Left Ctrl -> 0x3B.
        assert_eq!(mods_to_cgkeycodes(1 << 0, false), vec![0x3B]);
    }

    #[test]
    fn cmd_ctrl_swap_exchanges_them() {
        // With swap, Left Ctrl bit yields the Command keycode (0x37)...
        assert_eq!(mods_to_cgkeycodes(1 << 0, true), vec![0x37]);
        // ...and the Left Meta bit yields the Ctrl keycode (0x3B).
        assert_eq!(mods_to_cgkeycodes(1 << 3, true), vec![0x3B]);
    }

    #[test]
    fn mods_flags_fold_left_right_and_map_meta() {
        // Left or Right Command both set the Command flag.
        assert_eq!(
            mods_to_cgflags(1 << 3, false),
            CGEventFlags::CGEventFlagCommand
        );
        assert_eq!(
            mods_to_cgflags(1 << 7, false),
            CGEventFlags::CGEventFlagCommand
        );
        // Ctrl+Shift.
        let cs = mods_to_cgflags((1 << 0) | (1 << 1), false);
        assert!(cs.contains(CGEventFlags::CGEventFlagControl));
        assert!(cs.contains(CGEventFlags::CGEventFlagShift));
    }

    #[test]
    fn mods_flags_swap_ctrl_command() {
        // Ctrl bit yields the Command flag under swap.
        assert_eq!(
            mods_to_cgflags(1 << 0, true),
            CGEventFlags::CGEventFlagCommand
        );
        // Meta bit yields the Control flag under swap.
        assert_eq!(
            mods_to_cgflags(1 << 3, true),
            CGEventFlags::CGEventFlagControl
        );
    }

    #[test]
    fn supported_set_is_sorted_and_nonempty() {
        let v = supported_hid_usages();
        assert!(!v.is_empty());
        assert!(v.windows(2).all(|w| w[0] < w[1]));
        assert!(v.contains(&0x3A)); // F1
        assert!(v.contains(&0x85)); // Keypad comma
    }
}
