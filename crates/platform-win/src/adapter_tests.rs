use super::*;

#[test]
fn display_bounds_clamp_local_coords() {
    let bounds = DisplayBounds {
        id: 2,
        left: -1920,
        top: 100,
        width: 1920,
        height: 1080,
    };
    assert_eq!(bounds.local_to_virtual(20, 30), (-1900, 130));
    assert_eq!(bounds.local_to_virtual(-5, -8), (-1920, 100));
    assert_eq!(bounds.local_to_virtual(4000, 4000), (-1, 1179));
}

#[test]
fn button_indices_match_wire_catalog() {
    assert_eq!(button_of(0), Ok(Button::Left));
    assert_eq!(button_of(4), Ok(Button::Forward));
    assert_eq!(button_of(5), Err(UnknownButton(5)));
}

#[test]
fn modifier_bits_map_to_hid_usages() {
    assert_eq!(
        modifier_usages((1 << 0) | (1 << 3) | (1 << 7)),
        vec![0xE0, 0xE3, 0xE7]
    );
    assert!(modifier_usages(1 << 12).is_empty());
}
