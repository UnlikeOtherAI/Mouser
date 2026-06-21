//! Cross-platform keymap parity (audit H11 / C2-8).
//!
//! Asserts the macOS HID→CGKeyCode table, the Linux HID→evdev table, and the
//! Windows HID→scancode/VK table all cover the **same** set of USB HID usages
//! (Usage Page 0x07) with no divergence. Each crate exposes its coverage as a
//! host-independent `supported_hid_usages()` so this test runs on a macOS build
//! host even though `platform-linux`'s `Key` mapping and `platform-win`'s
//! `SendInput` injection are OS-specific.

#![cfg(target_os = "macos")]

use platform_linux::keymap as linux_keymap;
use platform_mac::keymap as mac_keymap;
use platform_win::keymap as win_keymap;

#[test]
fn mac_linux_and_win_cover_the_same_hid_usages() {
    let mac = mac_keymap::supported_hid_usages();
    let linux = linux_keymap::supported_hid_usages();
    let win = win_keymap::supported_hid_usages();

    // Identical coverage across all three platforms.
    assert_eq!(
        mac,
        linux,
        "mac and linux keymaps cover different HID usages\nmac-only: {:?}\nlinux-only: {:?}",
        diff(&mac, &linux),
        diff(&linux, &mac),
    );
    assert_eq!(
        mac,
        win,
        "mac and windows keymaps cover different HID usages\nmac-only: {:?}\nwin-only: {:?}",
        diff(&mac, &win),
        diff(&win, &mac),
    );
    // Transitively linux == win, but assert it directly for a clear failure.
    assert_eq!(
        linux,
        win,
        "linux and windows keymaps cover different HID usages\nlinux-only: {:?}\nwin-only: {:?}",
        diff(&linux, &win),
        diff(&win, &linux),
    );
}

#[test]
fn coverage_sets_are_sorted_and_collision_free() {
    for (name, set) in [
        ("mac", mac_keymap::supported_hid_usages()),
        ("linux", linux_keymap::supported_hid_usages()),
        ("win", win_keymap::supported_hid_usages()),
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
        (0x53u16, "NumLock"),
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
