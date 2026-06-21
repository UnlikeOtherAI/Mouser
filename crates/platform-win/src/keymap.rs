//! HID Usage (USB HID Usage Page 0x07) → Windows key identifiers.
//!
//! The Mouser wire protocol carries keys as **USB HID Usage IDs on Usage Page
//! 0x07** (see `docs/communication-interface.md` §7.5 + Appendix B). This module
//! is the **Windows half** of the bidirectional table Appendix B mandates each
//! platform adapter ship.
//!
//! ## Why scancodes, not virtual-key codes
//! `SendInput` can carry either a **virtual-key code** (`wVk`) or a **hardware
//! scancode** (`wScan` with `KEYEVENTF_SCANCODE`). VK codes are *layout-dependent*
//! at the OS layer (`A` is `0x41` but where that lands depends on the active
//! layout), whereas a **scancode names a physical key** — exactly the
//! "physical-key semantics" the wire spec requires (§7.5: "`usage` = USB HID
//! Usage Page 0x07 … physical-key semantics"). So the primary mapping is
//! HID usage → **PS/2 Set 1 scancode** (the "make" code), plus an *extended*
//! (`E0`-prefixed) flag for keys on the grey extended block (arrows, right-hand
//! modifiers, etc.). Injection then uses `KEYEVENTF_SCANCODE`
//! (+`KEYEVENTF_EXTENDEDKEY` when extended).
//!
//! A virtual-key fallback ([`hid_usage_to_vk`]) is provided for callers/tests
//! that prefer VK, but injection ([`crate::inject::key`]) uses scancodes.
//!
//! Only the common subset needed for the skeleton is mapped; unmapped usages
//! return `None`. The full table is filled in when this crate is reconciled with
//! `mouser-core`'s `InputInjection` trait.

/// A Windows physical-key identity: a PS/2 Set 1 **scancode** (the "make" code)
/// plus whether it lives on the `E0`-prefixed **extended** block.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScanCode {
    /// PS/2 Set 1 make code (low byte; the `E0` prefix is conveyed by
    /// [`Self::extended`], not folded into this value).
    pub code: u16,
    /// `true` for keys on the grey extended block (`E0`-prefixed): arrows, the
    /// right-hand Ctrl/Alt, the GUI/Win keys, navigation cluster, KP Enter, etc.
    /// Injection must set `KEYEVENTF_EXTENDEDKEY` for these.
    pub extended: bool,
}

impl ScanCode {
    const fn plain(code: u16) -> Self {
        Self {
            code,
            extended: false,
        }
    }
    const fn ext(code: u16) -> Self {
        Self {
            code,
            extended: true,
        }
    }
}

/// Translate a USB HID Usage (Usage Page 0x07) to a Windows PS/2 Set 1
/// [`ScanCode`] (make code + extended flag).
///
/// This is the canonical injection path: scancodes are layout-independent and
/// name the *physical* key, matching the wire spec's physical-key semantics
/// (§7.5). Returns `None` for usages not yet in the table.
#[must_use]
pub fn hid_usage_to_scancode(usage: u16) -> Option<ScanCode> {
    // Source of truth for scancodes: PS/2 Set 1 "make" codes (the values a
    // standard US keyboard controller emits). HID usage values: USB HID Usage
    // Tables, Page 0x07. Cross-checked against the USB HID→PS/2 translation
    // table (HUT 1.12 / "Keyboard/Keypad Page").
    let sc = match usage {
        // Letters a..z (HID 0x04..=0x1D)
        0x04 => ScanCode::plain(0x1E), // a
        0x05 => ScanCode::plain(0x30), // b
        0x06 => ScanCode::plain(0x2E), // c
        0x07 => ScanCode::plain(0x20), // d
        0x08 => ScanCode::plain(0x12), // e
        0x09 => ScanCode::plain(0x21), // f
        0x0A => ScanCode::plain(0x22), // g
        0x0B => ScanCode::plain(0x23), // h
        0x0C => ScanCode::plain(0x17), // i
        0x0D => ScanCode::plain(0x24), // j
        0x0E => ScanCode::plain(0x25), // k
        0x0F => ScanCode::plain(0x26), // l
        0x10 => ScanCode::plain(0x32), // m
        0x11 => ScanCode::plain(0x31), // n
        0x12 => ScanCode::plain(0x18), // o
        0x13 => ScanCode::plain(0x19), // p
        0x14 => ScanCode::plain(0x10), // q
        0x15 => ScanCode::plain(0x13), // r
        0x16 => ScanCode::plain(0x1F), // s
        0x17 => ScanCode::plain(0x14), // t
        0x18 => ScanCode::plain(0x16), // u
        0x19 => ScanCode::plain(0x2F), // v
        0x1A => ScanCode::plain(0x11), // w
        0x1B => ScanCode::plain(0x2D), // x
        0x1C => ScanCode::plain(0x15), // y
        0x1D => ScanCode::plain(0x2C), // z

        // Number row 1..0 (HID 0x1E..=0x27)
        0x1E => ScanCode::plain(0x02), // 1
        0x1F => ScanCode::plain(0x03), // 2
        0x20 => ScanCode::plain(0x04), // 3
        0x21 => ScanCode::plain(0x05), // 4
        0x22 => ScanCode::plain(0x06), // 5
        0x23 => ScanCode::plain(0x07), // 6
        0x24 => ScanCode::plain(0x08), // 7
        0x25 => ScanCode::plain(0x09), // 8
        0x26 => ScanCode::plain(0x0A), // 9
        0x27 => ScanCode::plain(0x0B), // 0

        // Whitespace / control
        0x28 => ScanCode::plain(0x1C), // Enter / Return
        0x29 => ScanCode::plain(0x01), // Escape
        0x2A => ScanCode::plain(0x0E), // Backspace
        0x2B => ScanCode::plain(0x0F), // Tab
        0x2C => ScanCode::plain(0x39), // Space

        // Punctuation (US layout physical keys)
        0x2D => ScanCode::plain(0x0C), // - and _
        0x2E => ScanCode::plain(0x0D), // = and +
        0x2F => ScanCode::plain(0x1A), // [ and {
        0x30 => ScanCode::plain(0x1B), // ] and }
        0x31 => ScanCode::plain(0x2B), // \ and |
        0x33 => ScanCode::plain(0x27), // ; and :
        0x34 => ScanCode::plain(0x28), // ' and "
        0x35 => ScanCode::plain(0x29), // ` and ~
        0x36 => ScanCode::plain(0x33), // , and <
        0x37 => ScanCode::plain(0x34), // . and >
        0x38 => ScanCode::plain(0x35), // / and ?
        0x39 => ScanCode::plain(0x3A), // Caps Lock

        // Function row F1..F12 (HID 0x3A..=0x45)
        0x3A => ScanCode::plain(0x3B), // F1
        0x3B => ScanCode::plain(0x3C), // F2
        0x3C => ScanCode::plain(0x3D), // F3
        0x3D => ScanCode::plain(0x3E), // F4
        0x3E => ScanCode::plain(0x3F), // F5
        0x3F => ScanCode::plain(0x40), // F6
        0x40 => ScanCode::plain(0x41), // F7
        0x41 => ScanCode::plain(0x42), // F8
        0x42 => ScanCode::plain(0x43), // F9
        0x43 => ScanCode::plain(0x44), // F10
        0x44 => ScanCode::plain(0x57), // F11
        0x45 => ScanCode::plain(0x58), // F12

        // Navigation cluster (extended, E0-prefixed) (HID 0x49..=0x4E)
        0x49 => ScanCode::ext(0x52), // Insert
        0x4A => ScanCode::ext(0x47), // Home
        0x4B => ScanCode::ext(0x49), // Page Up
        0x4C => ScanCode::ext(0x53), // Delete Forward
        0x4D => ScanCode::ext(0x4F), // End
        0x4E => ScanCode::ext(0x51), // Page Down

        // Arrows (extended) (HID 0x4F..=0x52)
        0x4F => ScanCode::ext(0x4D), // Right
        0x50 => ScanCode::ext(0x4B), // Left
        0x51 => ScanCode::ext(0x50), // Down
        0x52 => ScanCode::ext(0x48), // Up

        // Keypad (HID 0x53..=0x63, plus 0x67 KpEqual, 0x85 KpComma).
        // PS/2 Set 1 make codes. The numeric/`.` keypad keys are the *plain*
        // (non-extended) codes that the extended nav cluster (Insert/Home/…,
        // KpSlash) re-uses with the E0 prefix — so the only collision-sensitive
        // members here are KpSlash (0x54) and KpEnter (0x58), which ARE extended
        // (E0 0x35 / E0 0x1C) to disambiguate from `/` (0x35) and Enter (0x1C).
        0x53 => ScanCode::plain(0x45), // Num Lock
        0x54 => ScanCode::ext(0x35),   // Keypad / (extended: shares `/` 0x35)
        0x55 => ScanCode::plain(0x37), // Keypad *
        0x56 => ScanCode::plain(0x4A), // Keypad -
        0x57 => ScanCode::plain(0x4E), // Keypad +
        0x58 => ScanCode::ext(0x1C),   // Keypad Enter (extended: shares Enter 0x1C)
        0x59 => ScanCode::plain(0x4F), // Keypad 1 (shares End nav code, non-ext)
        0x5A => ScanCode::plain(0x50), // Keypad 2 (Down)
        0x5B => ScanCode::plain(0x51), // Keypad 3 (Page Down)
        0x5C => ScanCode::plain(0x4B), // Keypad 4 (Left)
        0x5D => ScanCode::plain(0x4C), // Keypad 5
        0x5E => ScanCode::plain(0x4D), // Keypad 6 (Right)
        0x5F => ScanCode::plain(0x47), // Keypad 7 (Home)
        0x60 => ScanCode::plain(0x48), // Keypad 8 (Up)
        0x61 => ScanCode::plain(0x49), // Keypad 9 (Page Up)
        0x62 => ScanCode::plain(0x52), // Keypad 0 (Insert)
        0x63 => ScanCode::plain(0x53), // Keypad . (Delete)
        0x67 => ScanCode::plain(0x59), // Keypad =
        0x85 => ScanCode::plain(0x7E), // Keypad , (Brazilian/ABNT2 numpad comma)

        // Modifiers (HID 0xE0..=0xE7).
        // Left-hand keys are plain; right-hand Ctrl/Alt and both GUI keys are
        // on the extended (E0) block.
        0xE0 => ScanCode::plain(0x1D), // Left Ctrl
        0xE1 => ScanCode::plain(0x2A), // Left Shift
        0xE2 => ScanCode::plain(0x38), // Left Alt
        0xE3 => ScanCode::ext(0x5B),   // Left GUI / Win
        0xE4 => ScanCode::ext(0x1D),   // Right Ctrl
        0xE5 => ScanCode::plain(0x36), // Right Shift
        0xE6 => ScanCode::ext(0x38),   // Right Alt / AltGr
        0xE7 => ScanCode::ext(0x5C),   // Right GUI / Win

        _ => return None,
    };
    Some(sc)
}

/// Translate a USB HID Usage (Usage Page 0x07) to a Windows **virtual-key
/// code** (`VK_*`, the values used by `wVk` in `KEYBDINPUT`).
///
/// This is a *secondary* mapping for callers/tests that want VK semantics; the
/// injection path uses [`hid_usage_to_scancode`] because scancodes are
/// layout-independent. Returns `None` for usages not yet in the table.
#[must_use]
#[allow(clippy::too_many_lines)]
pub fn hid_usage_to_vk(usage: u16) -> Option<u16> {
    // VK constants from Win32 `WinUser.h` (`VK_*`). Letters/digits use their
    // ASCII codes per the Win32 convention (no dedicated VK_A/VK_0 macros).
    let vk: u16 = match usage {
        // Letters a..z → 'A'..'Z' (0x41..=0x5A)
        0x04..=0x1D => 0x41 + (usage - 0x04),
        // Numbers 1..9 → '1'..'9' (0x31..=0x39), 0 → '0' (0x30)
        0x1E..=0x26 => 0x31 + (usage - 0x1E),
        0x27 => 0x30, // 0

        0x28 => 0x0D, // VK_RETURN
        0x29 => 0x1B, // VK_ESCAPE
        0x2A => 0x08, // VK_BACK
        0x2B => 0x09, // VK_TAB
        0x2C => 0x20, // VK_SPACE

        0x2D => 0xBD, // VK_OEM_MINUS
        0x2E => 0xBB, // VK_OEM_PLUS
        0x2F => 0xDB, // VK_OEM_4  [
        0x30 => 0xDD, // VK_OEM_6  ]
        0x31 => 0xDC, // VK_OEM_5  backslash
        0x33 => 0xBA, // VK_OEM_1  ;
        0x34 => 0xDE, // VK_OEM_7  '
        0x35 => 0xC0, // VK_OEM_3  `
        0x36 => 0xBC, // VK_OEM_COMMA
        0x37 => 0xBE, // VK_OEM_PERIOD
        0x38 => 0xBF, // VK_OEM_2  /
        0x39 => 0x14, // VK_CAPITAL

        // F1..F12 → VK_F1..VK_F12 (0x70..=0x7B)
        0x3A..=0x45 => 0x70 + (usage - 0x3A),

        0x49 => 0x2D, // VK_INSERT
        0x4A => 0x24, // VK_HOME
        0x4B => 0x21, // VK_PRIOR (Page Up)
        0x4C => 0x2E, // VK_DELETE
        0x4D => 0x23, // VK_END
        0x4E => 0x22, // VK_NEXT (Page Down)

        0x4F => 0x27, // VK_RIGHT
        0x50 => 0x25, // VK_LEFT
        0x51 => 0x28, // VK_DOWN
        0x52 => 0x26, // VK_UP

        // Keypad (HID 0x53..=0x63, plus 0x67 KpEqual, 0x85 KpComma).
        0x53 => 0x90, // VK_NUMLOCK
        0x54 => 0x6F, // VK_DIVIDE
        0x55 => 0x6A, // VK_MULTIPLY
        0x56 => 0x6D, // VK_SUBTRACT
        0x57 => 0x6B, // VK_ADD
        0x58 => 0x0D, // VK_RETURN (Keypad Enter shares VK_RETURN; the extended
        // scancode is what distinguishes it on the inject path)
        0x59 => 0x61, // VK_NUMPAD1
        0x5A => 0x62, // VK_NUMPAD2
        0x5B => 0x63, // VK_NUMPAD3
        0x5C => 0x64, // VK_NUMPAD4
        0x5D => 0x65, // VK_NUMPAD5
        0x5E => 0x66, // VK_NUMPAD6
        0x5F => 0x67, // VK_NUMPAD7
        0x60 => 0x68, // VK_NUMPAD8
        0x61 => 0x69, // VK_NUMPAD9
        0x62 => 0x60, // VK_NUMPAD0
        0x63 => 0x6E, // VK_DECIMAL (Keypad .)
        0x67 => 0x92, // VK_OEM_NEC_EQUAL (Keypad =)
        0x85 => 0xC2, // VK_ABNT_C2 (Keypad , on Brazilian ABNT2)

        0xE0 => 0xA2, // VK_LCONTROL
        0xE1 => 0xA0, // VK_LSHIFT
        0xE2 => 0xA4, // VK_LMENU (Left Alt)
        0xE3 => 0x5B, // VK_LWIN
        0xE4 => 0xA3, // VK_RCONTROL
        0xE5 => 0xA1, // VK_RSHIFT
        0xE6 => 0xA5, // VK_RMENU (Right Alt)
        0xE7 => 0x5C, // VK_RWIN

        _ => return None,
    };
    Some(vk)
}

/// The exact set of HID usages this Windows table maps, sorted ascending.
///
/// Host-independent pure data (mirrors the `match` arms of
/// [`hid_usage_to_scancode`] / [`hid_usage_to_vk`], which cover the same set), so
/// the cross-platform parity test can run on any build host even though the real
/// `SendInput` injection is Windows-only. Mirrored by
/// `platform_mac::keymap::supported_hid_usages` and
/// `platform_linux::keymap::supported_hid_usages`; a three-way parity test
/// asserts all three are identical (no collisions, same coverage — audit
/// H11/C2-8).
#[must_use]
pub fn supported_hid_usages() -> Vec<u16> {
    let mut v: Vec<u16> = Vec::new();
    v.extend(0x04u16..=0x39); // letters, number row, control, punctuation, caps
    v.extend(0x3Au16..=0x45); // F1..F12
    v.extend(0x49u16..=0x52); // nav cluster + arrows
    v.extend(0x53u16..=0x63); // keypad block
    v.push(0x67); // Keypad =
    v.push(0x85); // Keypad ,
    v.extend(0xE0u16..=0xE7); // modifiers
                              // 0x32 (HID "Non-US #") has no portable mac/evdev counterpart in this set.
    v.retain(|&u| u != 0x32);
    v.sort_unstable();
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_known_scancodes() {
        assert_eq!(hid_usage_to_scancode(0x04), Some(ScanCode::plain(0x1E))); // a
        assert_eq!(hid_usage_to_scancode(0x28), Some(ScanCode::plain(0x1C))); // Enter
        assert_eq!(hid_usage_to_scancode(0x29), Some(ScanCode::plain(0x01))); // Esc
        assert_eq!(hid_usage_to_scancode(0x2C), Some(ScanCode::plain(0x39))); // Space
    }

    #[test]
    fn arrows_and_right_modifiers_are_extended() {
        assert_eq!(hid_usage_to_scancode(0x4F), Some(ScanCode::ext(0x4D))); // Right arrow
        assert_eq!(hid_usage_to_scancode(0x52), Some(ScanCode::ext(0x48))); // Up arrow
        assert_eq!(hid_usage_to_scancode(0xE4), Some(ScanCode::ext(0x1D))); // Right Ctrl
        assert_eq!(hid_usage_to_scancode(0xE3), Some(ScanCode::ext(0x5B))); // Left Win
                                                                            // Left Ctrl/Shift/Alt are NOT extended.
        assert!(!hid_usage_to_scancode(0xE0).unwrap().extended);
        assert!(!hid_usage_to_scancode(0xE1).unwrap().extended);
        assert!(!hid_usage_to_scancode(0xE2).unwrap().extended);
    }

    #[test]
    fn unknown_scancode_is_none() {
        assert_eq!(hid_usage_to_scancode(0xFFFF), None);
        assert_eq!(hid_usage_to_scancode(0x00), None);
    }

    #[test]
    fn maps_known_vks() {
        assert_eq!(hid_usage_to_vk(0x04), Some(0x41)); // a → 'A'
        assert_eq!(hid_usage_to_vk(0x1D), Some(0x5A)); // z → 'Z'
        assert_eq!(hid_usage_to_vk(0x1E), Some(0x31)); // 1 → '1'
        assert_eq!(hid_usage_to_vk(0x27), Some(0x30)); // 0 → '0'
        assert_eq!(hid_usage_to_vk(0x28), Some(0x0D)); // Enter → VK_RETURN
        assert_eq!(hid_usage_to_vk(0x3A), Some(0x70)); // F1
        assert_eq!(hid_usage_to_vk(0x45), Some(0x7B)); // F12
        assert_eq!(hid_usage_to_vk(0xE3), Some(0x5B)); // Left Win → VK_LWIN
    }

    #[test]
    fn unknown_vk_is_none() {
        assert_eq!(hid_usage_to_vk(0xFFFF), None);
        assert_eq!(hid_usage_to_vk(0x00), None);
    }

    #[test]
    fn keypad_scancodes_disambiguate_from_nav_cluster() {
        // NumLock + the arithmetic/numeric keypad keys are PLAIN (non-extended);
        // KpSlash and KpEnter are EXTENDED so they don't collide with `/`/Enter.
        assert_eq!(hid_usage_to_scancode(0x53), Some(ScanCode::plain(0x45))); // NumLock
        assert_eq!(hid_usage_to_scancode(0x54), Some(ScanCode::ext(0x35))); // Keypad /
        assert_eq!(hid_usage_to_scancode(0x55), Some(ScanCode::plain(0x37))); // Keypad *
        assert_eq!(hid_usage_to_scancode(0x57), Some(ScanCode::plain(0x4E))); // Keypad +
        assert_eq!(hid_usage_to_scancode(0x58), Some(ScanCode::ext(0x1C))); // Keypad Enter
        assert_eq!(hid_usage_to_scancode(0x62), Some(ScanCode::plain(0x52))); // Keypad 0
        assert_eq!(hid_usage_to_scancode(0x63), Some(ScanCode::plain(0x53))); // Keypad .
        assert_eq!(hid_usage_to_scancode(0x67), Some(ScanCode::plain(0x59))); // Keypad =
        assert_eq!(hid_usage_to_scancode(0x85), Some(ScanCode::plain(0x7E))); // Keypad ,
                                                                              // The plain numeric keypad codes mirror the extended nav cluster codes
                                                                              // (same low byte, extended flag is what differs): Keypad 7 == Home code.
        assert_eq!(hid_usage_to_scancode(0x5F).map(|s| s.code), Some(0x47));
        assert!(!hid_usage_to_scancode(0x5F).unwrap().extended);
        assert_eq!(hid_usage_to_scancode(0x4A), Some(ScanCode::ext(0x47))); // Home (extended twin)
    }

    #[test]
    fn keypad_vks_are_mapped() {
        assert_eq!(hid_usage_to_vk(0x53), Some(0x90)); // VK_NUMLOCK
        assert_eq!(hid_usage_to_vk(0x54), Some(0x6F)); // VK_DIVIDE
        assert_eq!(hid_usage_to_vk(0x62), Some(0x60)); // VK_NUMPAD0
        assert_eq!(hid_usage_to_vk(0x60), Some(0x68)); // VK_NUMPAD8
        assert_eq!(hid_usage_to_vk(0x63), Some(0x6E)); // VK_DECIMAL
        assert_eq!(hid_usage_to_vk(0x58), Some(0x0D)); // Keypad Enter == VK_RETURN
    }

    #[test]
    fn supported_set_is_sorted_and_nonempty() {
        let v = supported_hid_usages();
        assert!(!v.is_empty());
        assert!(v.windows(2).all(|w| w[0] < w[1]));
        assert!(v.contains(&0x3A)); // F1
        assert!(v.contains(&0x4A)); // Home
        assert!(v.contains(&0x53)); // NumLock
        assert!(v.contains(&0x85)); // Keypad comma
        assert!(!v.contains(&0x32)); // excluded Non-US #
    }

    #[test]
    fn table_and_supported_set_agree() {
        // Every advertised usage maps in BOTH the scancode and VK tables...
        for u in supported_hid_usages() {
            assert!(
                hid_usage_to_scancode(u).is_some(),
                "supported usage {u:#06x} has no scancode mapping"
            );
            assert!(
                hid_usage_to_vk(u).is_some(),
                "supported usage {u:#06x} has no VK mapping"
            );
        }
        // ...and nothing outside the advertised set maps in either table.
        let set = supported_hid_usages();
        for u in 0x00u16..=0xFF {
            if hid_usage_to_scancode(u).is_some() {
                assert!(
                    set.contains(&u),
                    "scancode maps {u:#06x} but it is not in supported_hid_usages()"
                );
            }
            if hid_usage_to_vk(u).is_some() {
                assert!(
                    set.contains(&u),
                    "VK maps {u:#06x} but it is not in supported_hid_usages()"
                );
            }
        }
    }
}
