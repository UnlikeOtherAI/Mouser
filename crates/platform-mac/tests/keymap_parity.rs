//! Cross-platform keymap parity (audit H11).
//!
//! Asserts the macOS HID→CGKeyCode table and the Linux HID→evdev table cover the
//! **same** set of USB HID usages (Usage Page 0x07) with no divergence. Both
//! crates expose their coverage as host-independent `supported_hid_usages()` so
//! this test runs on a macOS build host even though `platform-linux`'s actual
//! `Key` mapping is Linux-only.

#![cfg(target_os = "macos")]

use platform_linux::keymap as linux_keymap;
use platform_mac::keymap as mac_keymap;

#[test]
fn mac_and_linux_cover_the_same_hid_usages() {
    let mac = mac_keymap::supported_hid_usages();
    let linux = linux_keymap::supported_hid_usages();

    // Identical coverage.
    assert_eq!(
        mac, linux,
        "mac and linux keymaps cover different HID usages\nmac-only: {:?}\nlinux-only: {:?}",
        diff(&mac, &linux),
        diff(&linux, &mac),
    );
}

#[test]
fn coverage_sets_are_sorted_and_collision_free() {
    for (name, set) in [
        ("mac", mac_keymap::supported_hid_usages()),
        ("linux", linux_keymap::supported_hid_usages()),
    ] {
        assert!(!set.is_empty(), "{name} coverage is empty");
        // Strictly increasing ⇒ sorted and no duplicate usages (no collisions).
        assert!(
            set.windows(2).all(|w| w[0] < w[1]),
            "{name} coverage is not strictly sorted / has duplicates"
        );
    }
}

#[test]
fn coverage_includes_the_expected_groups() {
    let mac = mac_keymap::supported_hid_usages();
    // A representative usage from each group the audit required.
    for (usage, what) in [
        (0x04u16, "letter a"),
        (0x1Eu16, "number 1"),
        (0x3Au16, "F1"),
        (0x45u16, "F12"),
        (0x4Au16, "Home"),
        (0x4Eu16, "PageDown"),
        (0x49u16, "Insert"),
        (0x4Cu16, "ForwardDelete"),
        (0x58u16, "Keypad Enter"),
        (0x62u16, "Keypad 0"),
        (0x85u16, "Keypad comma"),
        (0xE3u16, "Left Meta"),
    ] {
        assert!(mac.contains(&usage), "missing {what} ({usage:#06x})");
    }
}

fn diff(a: &[u16], b: &[u16]) -> Vec<u16> {
    a.iter().copied().filter(|u| !b.contains(u)).collect()
}
