use super::*;

use crate::adapter::DisplayBounds;
use mouser_core::platform::{CaptureDecision, InputSink, LocalInputEvent};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use windows::Win32::UI::WindowsAndMessaging::{
    LLKHF_EXTENDED, LLKHF_INJECTED, LLMHF_INJECTED, WM_KEYDOWN, WM_KEYUP, WM_LBUTTONDOWN,
    WM_MOUSEWHEEL, WM_XBUTTONUP, XBUTTON2,
};

#[derive(Default)]
struct RecordingSink {
    events: Mutex<Vec<LocalInputEvent>>,
}

impl RecordingSink {
    fn events(&self) -> Vec<LocalInputEvent> {
        lock_recover(&self.events).clone()
    }
}

impl InputSink for RecordingSink {
    fn on_event(&self, event: LocalInputEvent) -> CaptureDecision {
        lock_recover(&self.events).push(event);
        CaptureDecision::PassThrough
    }
}

fn install_test_sink(sink: &Arc<RecordingSink>) {
    let sink_trait: Arc<dyn InputSink> = sink.clone();
    lock_recover(capture_state()).sink = Some(sink_trait);
}

fn drain_capture_queue_for_test() {
    let events = {
        let mut pending = lock_recover(&capture_queue().pending);
        std::mem::take(&mut *pending)
    };
    for event in events {
        process_queued_capture_event(event);
    }
}

fn key(usage: u16, down: bool) -> LocalInputEvent {
    LocalInputEvent::Key {
        usage,
        down,
        mods: 0,
    }
}

fn cursor(x: i32, y: i32) -> LocalInputEvent {
    LocalInputEvent::CursorMoved {
        display_id: 0,
        x,
        y,
        dx: 0,
        dy: 0,
    }
}

#[test]
fn captured_keyboard_events_use_hid_usages() {
    let _guard = capture_test_lock();
    clear_capture_state();
    assert_eq!(
        keyboard_event_from_parts(WM_KEYDOWN, 0x1E, 0),
        Some(LocalInputEvent::Key {
            usage: 0x04,
            down: true,
            mods: 0,
        })
    );
    assert_eq!(
        keyboard_event_from_parts(WM_KEYUP, 0x1E, 0),
        Some(LocalInputEvent::Key {
            usage: 0x04,
            down: false,
            mods: 0,
        })
    );
    assert_eq!(
        keyboard_event_from_parts(WM_KEYDOWN, 0x1E, LLKHF_INJECTED.0),
        None
    );
    clear_capture_state();
}

#[test]
fn captured_keyboard_tracks_modifier_state() {
    let _guard = capture_test_lock();
    clear_capture_state();
    assert_eq!(
        keyboard_event_from_parts(WM_KEYDOWN, 0x1D, 0),
        Some(LocalInputEvent::Key {
            usage: 0xE0,
            down: true,
            mods: 1,
        })
    );
    assert_eq!(
        keyboard_event_from_parts(WM_KEYDOWN, 0x1E, 0),
        Some(LocalInputEvent::Key {
            usage: 0x04,
            down: true,
            mods: 1,
        })
    );
    assert_eq!(
        keyboard_event_from_parts(WM_KEYUP, 0x1D, 0),
        Some(LocalInputEvent::Key {
            usage: 0xE0,
            down: false,
            mods: 0,
        })
    );
    clear_capture_state();
}

#[test]
fn both_ctrl_chord_is_exposed_as_emergency_reclaim() {
    let _guard = capture_test_lock();
    clear_capture_state();
    assert!(!emergency_passthrough_active());
    let left = keyboard_event_from_parts(WM_KEYDOWN, 0x1D, 0).expect("left ctrl");
    assert!(!observe_emergency_reclaim(left));
    let right = keyboard_event_from_parts(WM_KEYDOWN, 0x1D, LLKHF_EXTENDED.0).expect("right ctrl");
    assert!(crate::capture::is_emergency_reclaim_event(right));
    assert!(observe_emergency_reclaim(right));
    assert!(emergency_passthrough_active());
    clear_capture_state();
}

#[test]
fn captured_mouse_buttons_and_wheel_map_to_core_events() {
    assert_eq!(
        mouse_event_from_parts(WM_LBUTTONDOWN, 0, 0),
        Some(LocalInputEvent::Button {
            button: 0,
            down: true,
        })
    );
    assert_eq!(
        mouse_event_from_parts(WM_XBUTTONUP, u32::from(XBUTTON2) << 16, 0),
        Some(LocalInputEvent::Button {
            button: 4,
            down: false,
        })
    );

    let negative_wheel = u32::from((-120_i16) as u16) << 16;
    assert_eq!(
        mouse_event_from_parts(WM_MOUSEWHEEL, negative_wheel, 0),
        Some(LocalInputEvent::Scroll { dx: 0, dy: -120 })
    );
    assert_eq!(
        mouse_event_from_parts(WM_MOUSEWHEEL, negative_wheel, LLMHF_INJECTED),
        None
    );
}

#[test]
fn captured_cursor_resolves_virtual_point_to_display_local_coords() {
    let _guard = capture_test_lock();
    clear_capture_state();
    lock_recover(capture_state()).displays = vec![DisplayBounds {
        id: 7,
        left: -100,
        top: 50,
        width: 200,
        height: 100,
    }];

    assert_eq!(
        cursor_event_for_virtual_point(-10, 80),
        LocalInputEvent::CursorMoved {
            display_id: 7,
            x: 90,
            y: 30,
            dx: 0,
            dy: 0,
        }
    );
    clear_capture_state();
}

#[test]
fn coalescing_preserves_interleaved_key_transitions() {
    let _guard = capture_test_lock();
    clear_capture_state();
    let sink = Arc::new(RecordingSink::default());
    install_test_sink(&sink);

    enqueue_cursor_capture_point(1, 1);
    enqueue_capture_event(key(0x04, true));
    enqueue_cursor_capture_point(2, 2);
    enqueue_cursor_capture_point(3, 3);
    enqueue_capture_event(key(0x04, false));
    enqueue_cursor_capture_point(4, 4);
    drain_capture_queue_for_test();

    let events = sink.events();
    let keys: Vec<LocalInputEvent> = events
        .iter()
        .copied()
        .filter(|e| matches!(e, LocalInputEvent::Key { .. }))
        .collect();
    assert_eq!(keys, vec![key(0x04, true), key(0x04, false)]);
    assert!(!events.contains(&cursor(2, 2)));
    assert!(events.contains(&cursor(3, 3)) && events.contains(&cursor(4, 4)));
    clear_capture_state();
}

#[test]
fn high_rate_cursor_flood_never_overflows_or_evicts_transitions() {
    let _guard = capture_test_lock();
    clear_capture_state();
    let sink = Arc::new(RecordingSink::default());
    install_test_sink(&sink);

    enqueue_capture_event(key(0x04, true));
    for i in 0..(MAX_CAPTURE_QUEUE as i32 * 8) {
        enqueue_cursor_capture_point(i, i);
    }
    enqueue_capture_event(key(0x04, false));

    {
        let pending = lock_recover(&capture_queue().pending);
        assert!(pending.len() <= 3);
    }

    drain_capture_queue_for_test();
    let events = sink.events();
    assert!(events.contains(&key(0x04, true)));
    assert!(events.contains(&key(0x04, false)));
    clear_capture_state();
}

#[test]
fn slow_sink_overflow_is_bounded_and_evicts_cursors_before_transitions() {
    let _guard = capture_test_lock();
    clear_capture_state();

    let pairs = MAX_CAPTURE_QUEUE * 3 / 4;
    enqueue_capture_event(key(0x04, true));
    for i in 0..pairs as i32 {
        enqueue_cursor_capture_point(i, i);
        enqueue_capture_event(LocalInputEvent::Button {
            button: 2,
            down: i % 2 == 0,
        });
    }

    {
        let pending = lock_recover(&capture_queue().pending);
        assert!(pending.len() <= MAX_CAPTURE_QUEUE);
        assert!(pending
            .iter()
            .any(|e| matches!(e, QueuedCaptureEvent::Event(LocalInputEvent::Key { .. }))));
        let transitions = pending
            .iter()
            .filter(|e| matches!(e, QueuedCaptureEvent::Event(LocalInputEvent::Button { .. })))
            .count();
        assert_eq!(transitions, pairs);
    }
    clear_capture_state();
}

#[test]
fn overflow_eviction_prefers_cursors_then_falls_back_to_front() {
    let mut q: VecDeque<QueuedCaptureEvent> = VecDeque::new();
    q.push_back(QueuedCaptureEvent::Event(key(0x04, true)));
    q.push_back(QueuedCaptureEvent::CursorPoint { x: 1, y: 1 });
    q.push_back(QueuedCaptureEvent::Event(LocalInputEvent::Button {
        button: 0,
        down: true,
    }));
    q.push_back(QueuedCaptureEvent::CursorPoint { x: 2, y: 2 });
    drop_one_for_overflow(&mut q);
    assert!(matches!(
        q.front(),
        Some(QueuedCaptureEvent::Event(LocalInputEvent::Key { .. }))
    ));
    assert_eq!(
        q.iter()
            .filter(|e| queued_capture_event_is_cursor(e))
            .count(),
        1
    );

    let mut all_transitions: VecDeque<QueuedCaptureEvent> = VecDeque::new();
    all_transitions.push_back(QueuedCaptureEvent::Event(key(0x04, true)));
    all_transitions.push_back(QueuedCaptureEvent::Event(key(0x05, true)));
    drop_one_for_overflow(&mut all_transitions);
    assert_eq!(all_transitions.len(), 1);
}

#[test]
fn raw_mouse_hook_points_are_converted_by_worker() {
    let _guard = capture_test_lock();
    clear_capture_state();
    lock_recover(capture_state()).displays = vec![DisplayBounds {
        id: 3,
        left: 100,
        top: 200,
        width: 500,
        height: 400,
    }];
    let sink = Arc::new(RecordingSink::default());
    install_test_sink(&sink);

    enqueue_cursor_capture_point(125, 250);
    drain_capture_queue_for_test();

    assert_eq!(
        sink.events(),
        vec![LocalInputEvent::CursorMoved {
            display_id: 3,
            x: 25,
            y: 50,
            dx: 0,
            dy: 0,
        }]
    );
    clear_capture_state();
}
