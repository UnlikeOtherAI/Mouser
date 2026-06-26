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
fn monitor_order_promotes_primary_to_display_zero() {
    let mut monitors = vec![
        MonitorRect {
            rect: RECT {
                left: -1920,
                top: 0,
                right: 0,
                bottom: 1080,
            },
            primary: false,
        },
        MonitorRect {
            rect: RECT {
                left: 0,
                top: -1200,
                right: 1920,
                bottom: 0,
            },
            primary: false,
        },
        MonitorRect {
            rect: RECT {
                left: 0,
                top: 0,
                right: 2560,
                bottom: 1440,
            },
            primary: true,
        },
        MonitorRect {
            rect: RECT {
                left: 2560,
                top: 0,
                right: 4480,
                bottom: 1080,
            },
            primary: false,
        },
    ];

    order_monitors(&mut monitors);

    let order: Vec<(bool, i32)> = monitors
        .iter()
        .map(|monitor| (monitor.primary, monitor.rect.left))
        .collect();
    assert_eq!(
        order,
        vec![(true, 0), (false, 0), (false, -1920), (false, 2560)]
    );
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

#[test]
fn cursor_hide_then_show_round_trips() {
    // Visible -> hide: save the live position and park.
    let (saved, action) = next_cursor_state(None, false, Some((100, 200)));
    assert_eq!(saved, Some((100, 200)));
    assert_eq!(action, CursorAction::Park);

    // Hidden -> show: restore that exact position and clear the save.
    let (saved, action) = next_cursor_state(saved, true, None);
    assert_eq!(saved, None);
    assert_eq!(action, CursorAction::Restore((100, 200)));
}

#[test]
fn cursor_double_hide_keeps_original_saved_position() {
    // First hide saves (100, 200).
    let (saved, _) = next_cursor_state(None, false, Some((100, 200)));
    // A second hide must NOT overwrite the save with the parked corner — the
    // current live position would by then be the corner, losing the real spot.
    let (saved, action) = next_cursor_state(saved, false, Some((9999, 9999)));
    assert_eq!(saved, Some((100, 200)));
    assert_eq!(action, CursorAction::None);
}

#[test]
fn cursor_double_show_is_a_noop() {
    // Already visible (None): showing again does nothing and stays visible.
    let (saved, action) = next_cursor_state(None, true, None);
    assert_eq!(saved, None);
    assert_eq!(action, CursorAction::None);
}
