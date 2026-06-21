//! HID Usage (USB HID Usage Page 0x07) → macOS virtual key code (`CGKeyCode`).
//!
//! The Mouser wire protocol carries keys as **USB HID Usage IDs on Usage Page
//! 0x07** (see `docs/communication-interface.md` §7.5 + Appendix B). macOS
//! injection (`CGEventCreateKeyboardEvent`) wants a *virtual key code*
//! (`CGKeyCode`, the Carbon/`Events.h` codes). This module is the macOS half of
//! the bidirectional table Appendix B mandates each platform adapter ship.
//!
//! Only the common subset needed for the spike is mapped; unmapped usages
//! return `None`. The full table is filled in when this crate is reconciled
//! with `mouser-core`'s `InputInjection` trait.

use core_graphics::event::CGKeyCode;

/// Translate a USB HID Usage (Usage Page 0x07) to a macOS `CGKeyCode`.
///
/// Returns `None` for usages not yet in the table.
#[must_use]
pub fn hid_usage_to_cgkeycode(usage: u16) -> Option<CGKeyCode> {
    // Source of truth for CGKeyCode values: core-graphics `KeyCode` constants
    // (Carbon Events.h). HID usage values: USB HID Usage Tables, Page 0x07.
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

        // Arrows (HID 0x4F..=0x52)
        0x4F => 0x7C, // Right
        0x50 => 0x7B, // Left
        0x51 => 0x7D, // Down
        0x52 => 0x7E, // Up

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
    fn unknown_usage_is_none() {
        assert_eq!(hid_usage_to_cgkeycode(0xFFFF), None);
        assert_eq!(hid_usage_to_cgkeycode(0x00), None);
    }
}
