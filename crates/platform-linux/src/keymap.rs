//! HID Usage (USB HID Usage Page 0x07) → Linux evdev `KEY_*` (`input_linux::Key`).
//!
//! The Mouser wire protocol carries keys as **USB HID Usage IDs on Usage Page
//! 0x07** (see `docs/communication-interface.md` §7.5 + Appendix B); uinput wants
//! a Linux `KEY_*` code. This module is the Linux half of the bidirectional table
//! Appendix B mandates each platform adapter ship, covering the same HID-usage
//! set as `platform-mac` (letters, number row, whitespace/control, punctuation,
//! arrows, modifiers, F1–F12, the nav cluster, and the keypad — audit H11).
//!
//! The real HID→`Key` map is Linux-only ([`hid_usage_to_evdev`]); the *set* of
//! covered usages ([`supported_hid_usages`]) is host-independent pure data so the
//! cross-platform parity test can run on a macOS build host.

#[cfg(target_os = "linux")]
use input_linux::Key;

/// Translate a USB HID Usage (Usage Page 0x07) to a Linux evdev [`Key`].
///
/// Returns `None` for usages not in the table. Linux-only: `Key` does not exist
/// on other hosts (the crate is a cfg-stub there).
#[cfg(target_os = "linux")]
#[must_use]
pub fn hid_usage_to_evdev(usage: u16) -> Option<Key> {
    let key = match usage {
        // Letters a..z (HID 0x04..=0x1D)
        0x04 => Key::A,
        0x05 => Key::B,
        0x06 => Key::C,
        0x07 => Key::D,
        0x08 => Key::E,
        0x09 => Key::F,
        0x0A => Key::G,
        0x0B => Key::H,
        0x0C => Key::I,
        0x0D => Key::J,
        0x0E => Key::K,
        0x0F => Key::L,
        0x10 => Key::M,
        0x11 => Key::N,
        0x12 => Key::O,
        0x13 => Key::P,
        0x14 => Key::Q,
        0x15 => Key::R,
        0x16 => Key::S,
        0x17 => Key::T,
        0x18 => Key::U,
        0x19 => Key::V,
        0x1A => Key::W,
        0x1B => Key::X,
        0x1C => Key::Y,
        0x1D => Key::Z,

        // Number row 1..0 (HID 0x1E..=0x27)
        0x1E => Key::Num1,
        0x1F => Key::Num2,
        0x20 => Key::Num3,
        0x21 => Key::Num4,
        0x22 => Key::Num5,
        0x23 => Key::Num6,
        0x24 => Key::Num7,
        0x25 => Key::Num8,
        0x26 => Key::Num9,
        0x27 => Key::Num0,

        // Whitespace / control
        0x28 => Key::Enter,
        0x29 => Key::Esc,
        0x2A => Key::Backspace,
        0x2B => Key::Tab,
        0x2C => Key::Space,

        // Punctuation
        0x2D => Key::Minus,
        0x2E => Key::Equal,
        0x2F => Key::LeftBrace,
        0x30 => Key::RightBrace,
        0x31 => Key::Backslash,
        0x33 => Key::Semicolon,
        0x34 => Key::Apostrophe,
        0x35 => Key::Grave,
        0x36 => Key::Comma,
        0x37 => Key::Dot,
        0x38 => Key::Slash,
        0x39 => Key::CapsLock,

        // Function row F1..F12 (HID 0x3A..=0x45)
        0x3A => Key::F1,
        0x3B => Key::F2,
        0x3C => Key::F3,
        0x3D => Key::F4,
        0x3E => Key::F5,
        0x3F => Key::F6,
        0x40 => Key::F7,
        0x41 => Key::F8,
        0x42 => Key::F9,
        0x43 => Key::F10,
        0x44 => Key::F11,
        0x45 => Key::F12,

        // Navigation cluster (HID 0x49..=0x4E)
        0x49 => Key::Insert,
        0x4A => Key::Home,
        0x4B => Key::PageUp,
        0x4C => Key::Delete, // Delete Forward
        0x4D => Key::End,
        0x4E => Key::PageDown,

        // Arrows (HID 0x4F..=0x52)
        0x4F => Key::Right,
        0x50 => Key::Left,
        0x51 => Key::Down,
        0x52 => Key::Up,

        // Keypad (HID 0x53..=0x63, plus 0x67 KpEqual, 0x85 KpComma)
        0x53 => Key::NumLock,
        0x54 => Key::KpSlash,
        0x55 => Key::KpAsterisk,
        0x56 => Key::KpMinus,
        0x57 => Key::KpPlus,
        0x58 => Key::KpEnter,
        0x59 => Key::Kp1,
        0x5A => Key::Kp2,
        0x5B => Key::Kp3,
        0x5C => Key::Kp4,
        0x5D => Key::Kp5,
        0x5E => Key::Kp6,
        0x5F => Key::Kp7,
        0x60 => Key::Kp8,
        0x61 => Key::Kp9,
        0x62 => Key::Kp0,
        0x63 => Key::KpDot,
        0x67 => Key::KpEqual,
        0x85 => Key::KpComma,

        // Modifiers (HID 0xE0..=0xE7)
        0xE0 => Key::LeftCtrl,
        0xE1 => Key::LeftShift,
        0xE2 => Key::LeftAlt,
        0xE3 => Key::LeftMeta,
        0xE4 => Key::RightCtrl,
        0xE5 => Key::RightShift,
        0xE6 => Key::RightAlt,
        0xE7 => Key::RightMeta,

        _ => return None,
    };
    Some(key)
}

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
