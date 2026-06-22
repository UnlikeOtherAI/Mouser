//! HID Usage (USB HID Usage Page 0x07) ↔ Linux evdev `KEY_*` (`input_linux::Key`).
//!
//! The Mouser wire protocol carries keys as **USB HID Usage IDs on Usage Page
//! 0x07** (see `docs/communication-interface.md` §7.5 + Appendix B); evdev/uinput
//! want a Linux `KEY_*` code. This module is the Linux half of the bidirectional
//! table Appendix B mandates each platform adapter ship, covering the same
//! HID-usage set as `platform-mac` (letters, number row, whitespace/control,
//! punctuation, arrows, modifiers, F1–F12, the nav cluster, and the keypad —
//! audit H11).
//!
//! The canonical mapping is a single host-independent table keyed by HID usage
//! ([`HID_TO_EVDEV`], pairs of `(hid_usage, evdev KEY_* code)`). Both directions
//! derive from it:
//! - injection ([`hid_usage_to_evdev`], Linux-only `Key`) for `UinputInjector`;
//! - capture ([`evdev_code_to_hid_usage`], host-independent `u16`) for
//!   `LinuxCapture`.
//!
//! Keeping the table host-independent (raw `u16` evdev codes, not `Key` — which
//! only exists on Linux) lets the cross-platform round-trip parity test run on a
//! macOS build host.

#[cfg(target_os = "linux")]
use input_linux::Key;

/// Canonical HID-usage → evdev `KEY_*` code table (Appendix B, audit H11).
///
/// `(hid_usage, evdev_code)` pairs. Evdev codes are the stable Linux kernel
/// `KEY_*` constants (`input-event-codes.h`), identical to the numeric values of
/// the corresponding `input_linux::Key` variants. Host-independent pure data so
/// the parity test builds everywhere.
const HID_TO_EVDEV: &[(u16, u16)] = &[
    // Letters a..z (HID 0x04..=0x1D)
    (0x04, 30), // A
    (0x05, 48), // B
    (0x06, 46), // C
    (0x07, 32), // D
    (0x08, 18), // E
    (0x09, 33), // F
    (0x0A, 34), // G
    (0x0B, 35), // H
    (0x0C, 23), // I
    (0x0D, 36), // J
    (0x0E, 37), // K
    (0x0F, 38), // L
    (0x10, 50), // M
    (0x11, 49), // N
    (0x12, 24), // O
    (0x13, 25), // P
    (0x14, 16), // Q
    (0x15, 19), // R
    (0x16, 31), // S
    (0x17, 20), // T
    (0x18, 22), // U
    (0x19, 47), // V
    (0x1A, 17), // W
    (0x1B, 45), // X
    (0x1C, 21), // Y
    (0x1D, 44), // Z
    // Number row 1..0 (HID 0x1E..=0x27)
    (0x1E, 2),  // 1
    (0x1F, 3),  // 2
    (0x20, 4),  // 3
    (0x21, 5),  // 4
    (0x22, 6),  // 5
    (0x23, 7),  // 6
    (0x24, 8),  // 7
    (0x25, 9),  // 8
    (0x26, 10), // 9
    (0x27, 11), // 0
    // Whitespace / control
    (0x28, 28), // Enter
    (0x29, 1),  // Esc
    (0x2A, 14), // Backspace
    (0x2B, 15), // Tab
    (0x2C, 57), // Space
    // Punctuation
    (0x2D, 12), // Minus
    (0x2E, 13), // Equal
    (0x2F, 26), // LeftBrace
    (0x30, 27), // RightBrace
    (0x31, 43), // Backslash
    (0x33, 39), // Semicolon
    (0x34, 40), // Apostrophe
    (0x35, 41), // Grave
    (0x36, 51), // Comma
    (0x37, 52), // Dot
    (0x38, 53), // Slash
    (0x39, 58), // CapsLock
    // Function row F1..F12 (HID 0x3A..=0x45)
    (0x3A, 59), // F1
    (0x3B, 60), // F2
    (0x3C, 61), // F3
    (0x3D, 62), // F4
    (0x3E, 63), // F5
    (0x3F, 64), // F6
    (0x40, 65), // F7
    (0x41, 66), // F8
    (0x42, 67), // F9
    (0x43, 68), // F10
    (0x44, 87), // F11
    (0x45, 88), // F12
    // Navigation cluster (HID 0x49..=0x4E)
    (0x49, 110), // Insert
    (0x4A, 102), // Home
    (0x4B, 104), // PageUp
    (0x4C, 111), // Delete (Delete Forward)
    (0x4D, 107), // End
    (0x4E, 109), // PageDown
    // Arrows (HID 0x4F..=0x52)
    (0x4F, 106), // Right
    (0x50, 105), // Left
    (0x51, 108), // Down
    (0x52, 103), // Up
    // Keypad (HID 0x53..=0x63, plus 0x67 KpEqual, 0x85 KpComma)
    (0x53, 69),  // NumLock
    (0x54, 98),  // KpSlash
    (0x55, 55),  // KpAsterisk
    (0x56, 74),  // KpMinus
    (0x57, 78),  // KpPlus
    (0x58, 96),  // KpEnter
    (0x59, 79),  // Kp1
    (0x5A, 80),  // Kp2
    (0x5B, 81),  // Kp3
    (0x5C, 75),  // Kp4
    (0x5D, 76),  // Kp5
    (0x5E, 77),  // Kp6
    (0x5F, 71),  // Kp7
    (0x60, 72),  // Kp8
    (0x61, 73),  // Kp9
    (0x62, 82),  // Kp0
    (0x63, 83),  // KpDot
    (0x67, 117), // KpEqual
    (0x85, 121), // KpComma
    // Modifiers (HID 0xE0..=0xE7)
    (0xE0, 29),  // LeftCtrl
    (0xE1, 42),  // LeftShift
    (0xE2, 56),  // LeftAlt
    (0xE3, 125), // LeftMeta
    (0xE4, 97),  // RightCtrl
    (0xE5, 54),  // RightShift
    (0xE6, 100), // RightAlt
    (0xE7, 126), // RightMeta
];

/// Translate a USB HID Usage (Usage Page 0x07) to the raw evdev `KEY_*` code.
///
/// Returns `None` for usages not in [`HID_TO_EVDEV`]. Host-independent (raw
/// `u16`), so it builds on every host and backs the parity test.
#[must_use]
pub fn hid_usage_to_evdev_code(usage: u16) -> Option<u16> {
    HID_TO_EVDEV
        .iter()
        .find_map(|&(hid, code)| (hid == usage).then_some(code))
}

/// Reverse of [`hid_usage_to_evdev_code`]: a captured evdev `KEY_*` code → its
/// HID usage (Usage Page 0x07), or `None` if the code is outside the canonical
/// table.
///
/// Host-independent (raw `u16`), used by capture ([`crate::capture::LinuxCapture`])
/// to report locally-observed keys as HID usages. Linear over the (small) table
/// on the cold capture path.
#[must_use]
pub fn evdev_code_to_hid_usage(code: u16) -> Option<u16> {
    HID_TO_EVDEV
        .iter()
        .find_map(|&(hid, c)| (c == code).then_some(hid))
}

/// Translate a USB HID Usage (Usage Page 0x07) to a Linux evdev [`Key`].
///
/// Returns `None` for usages not in [`HID_TO_EVDEV`] (or whose code is out of the
/// kernel's `Key` range, which never happens for this curated table). Linux-only:
/// `Key` does not exist on other hosts (the crate is a cfg-stub there).
#[cfg(target_os = "linux")]
#[must_use]
pub fn hid_usage_to_evdev(usage: u16) -> Option<Key> {
    hid_usage_to_evdev_code(usage).and_then(|code| Key::from_code(code).ok())
}

/// Reverse of [`hid_usage_to_evdev`]: a captured [`Key`] → its HID usage (Usage
/// Page 0x07), or `None` if the key is outside the canonical table. Linux-only
/// ([`Key`]).
#[cfg(target_os = "linux")]
#[must_use]
pub fn evdev_key_to_hid_usage(key: Key) -> Option<u16> {
    evdev_code_to_hid_usage(key.code())
}

/// Map a captured pointer-button evdev code (`BTN_*`) to the wire button index
/// (§7.5: 0=left, 1=right, 2=middle, 3=back, 4=forward), or `None` for buttons
/// outside the §7.5 set. Host-independent (raw `u16` codes).
#[must_use]
pub fn evdev_btn_to_button(code: u16) -> Option<u8> {
    match code {
        0x110 => Some(0), // BTN_LEFT
        0x111 => Some(1), // BTN_RIGHT
        0x112 => Some(2), // BTN_MIDDLE
        0x113 => Some(3), // BTN_SIDE  (back)
        0x114 => Some(4), // BTN_EXTRA (forward)
        _ => None,
    }
}

/// Evdev relative-axis code: vertical wheel (`REL_WHEEL`, detents).
pub const REL_WHEEL: u16 = 0x08;
/// Evdev relative-axis code: horizontal wheel (`REL_HWHEEL`, detents).
pub const REL_HWHEEL: u16 = 0x06;
/// Evdev relative-axis code: vertical pointer motion (`REL_Y`).
pub const REL_Y: u16 = 0x01;
/// Evdev relative-axis code: horizontal pointer motion (`REL_X`).
pub const REL_X: u16 = 0x00;

/// The exact set of HID usages this Linux table maps, sorted ascending.
///
/// Host-independent pure data (matches the `match` arms of
/// [`hid_usage_to_evdev`], asserted by a Linux-only test). Mirrored by
/// `platform_mac::keymap::supported_hid_usages`; a cross-platform parity test
/// asserts the two are identical (audit H11).
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

/// Every evdev [`Key`] the canonical keymap can emit, for declaring the uinput
/// device's keybits at creation. Linux-only ([`Key`]).
#[cfg(target_os = "linux")]
#[must_use]
pub fn all_evdev_keys() -> Vec<Key> {
    supported_hid_usages()
        .into_iter()
        .filter_map(hid_usage_to_evdev)
        .collect()
}

/// Translate the wire `mods` bitmask (Appendix B) into the evdev modifier keys
/// that are held down.
///
/// Bit order (Appendix B): 0 LCtrl, 1 LShift, 2 LAlt, 3 LMeta, 4 RCtrl,
/// 5 RShift, 6 RAlt, 7 RMeta. `Meta` maps to the Super/Win key (`LeftMeta`/
/// `RightMeta`). Unknown bits are ignored. Returned in bit order so press/release
/// is deterministic. Linux-only ([`Key`]).
#[cfg(target_os = "linux")]
#[must_use]
pub fn mods_to_evdev(mods: u16) -> Vec<Key> {
    const MOD_BITS: [(u16, u16); 8] = [
        (0, 0xE0),
        (1, 0xE1),
        (2, 0xE2),
        (3, 0xE3),
        (4, 0xE4),
        (5, 0xE5),
        (6, 0xE6),
        (7, 0xE7),
    ];
    let mut out = Vec::new();
    for (bit, usage) in MOD_BITS {
        if mods & (1 << bit) != 0 {
            if let Some(k) = hid_usage_to_evdev(usage) {
                out.push(k);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn supported_set_is_sorted_and_nonempty() {
        let v = supported_hid_usages();
        assert!(!v.is_empty());
        assert!(v.windows(2).all(|w| w[0] < w[1]));
        assert!(v.contains(&0x3A)); // F1
        assert!(v.contains(&0x4A)); // Home
        assert!(v.contains(&0x85)); // Keypad comma
        assert!(!v.contains(&0x32)); // excluded Non-US #
    }

    /// Host-independent capture↔injection round-trip parity (runs on macOS too):
    /// every HID usage that maps to an evdev code maps back to the **same** HID
    /// usage. Guards against a typo desyncing the forward and reverse tables.
    #[test]
    fn hid_evdev_round_trip_is_identity() {
        for u in supported_hid_usages() {
            let code = hid_usage_to_evdev_code(u)
                .unwrap_or_else(|| panic!("supported usage {u:#06x} has no evdev code"));
            assert_eq!(
                evdev_code_to_hid_usage(code),
                Some(u),
                "usage {u:#06x} -> code {code} did not round-trip"
            );
        }
    }

    /// The canonical table is an injective map: no two HID usages share an evdev
    /// code (otherwise the reverse map would be ambiguous and lose keys).
    #[test]
    fn evdev_codes_are_unique() {
        let mut codes: Vec<u16> = HID_TO_EVDEV.iter().map(|&(_, c)| c).collect();
        codes.sort_unstable();
        let len_with_dups = codes.len();
        codes.dedup();
        assert_eq!(len_with_dups, codes.len(), "duplicate evdev code in table");
    }

    /// The canonical table covers exactly the advertised usage set.
    #[test]
    fn table_covers_supported_set() {
        let mut table: Vec<u16> = HID_TO_EVDEV.iter().map(|&(hid, _)| hid).collect();
        table.sort_unstable();
        assert_eq!(table, supported_hid_usages());
    }

    /// Pointer-button reverse mapping (§7.5) is host-independent and total over
    /// the five defined buttons; anything else is `None`.
    #[test]
    fn evdev_buttons_map_to_wire_indices() {
        assert_eq!(evdev_btn_to_button(0x110), Some(0)); // left
        assert_eq!(evdev_btn_to_button(0x111), Some(1)); // right
        assert_eq!(evdev_btn_to_button(0x112), Some(2)); // middle
        assert_eq!(evdev_btn_to_button(0x113), Some(3)); // back
        assert_eq!(evdev_btn_to_button(0x114), Some(4)); // forward
        assert_eq!(evdev_btn_to_button(0x115), None); // BTN_FORWARD (unused here)
        assert_eq!(evdev_btn_to_button(0x00), None);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn table_and_supported_set_agree() {
        // Every advertised usage maps...
        for u in supported_hid_usages() {
            assert!(
                hid_usage_to_evdev(u).is_some(),
                "supported usage {u:#06x} has no evdev mapping"
            );
        }
        // ...and nothing outside the advertised set maps.
        let set = supported_hid_usages();
        for u in 0x00u16..=0xFF {
            if hid_usage_to_evdev(u).is_some() {
                assert!(
                    set.contains(&u),
                    "evdev maps {u:#06x} but it is not in supported_hid_usages()"
                );
            }
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn maps_representative_keys() {
        assert_eq!(hid_usage_to_evdev(0x04), Some(Key::A));
        assert_eq!(hid_usage_to_evdev(0x3A), Some(Key::F1));
        assert_eq!(hid_usage_to_evdev(0x4A), Some(Key::Home));
        assert_eq!(hid_usage_to_evdev(0x58), Some(Key::KpEnter));
        assert_eq!(hid_usage_to_evdev(0xE3), Some(Key::LeftMeta));
        assert_eq!(hid_usage_to_evdev(0xFFFF), None);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn mods_map_meta_to_super() {
        assert_eq!(mods_to_evdev(1 << 3), vec![Key::LeftMeta]);
        assert_eq!(mods_to_evdev(1 << 0), vec![Key::LeftCtrl]);
        assert_eq!(mods_to_evdev(1 << 7), vec![Key::RightMeta]);
    }
}
