use super::*;

struct RecordingSink {
    decision: CaptureDecision,
    seen: Mutex<Vec<LocalInputEvent>>,
}

impl RecordingSink {
    fn new(decision: CaptureDecision) -> Self {
        Self {
            decision,
            seen: Mutex::new(Vec::new()),
        }
    }
}

impl InputSink for RecordingSink {
    fn on_event(&self, event: LocalInputEvent) -> CaptureDecision {
        self.seen.lock().expect("seen").push(event);
        self.decision
    }
}

struct PanickingSink;

impl InputSink for PanickingSink {
    fn on_event(&self, _event: LocalInputEvent) -> CaptureDecision {
        panic!("sink panic")
    }
}

fn reclaim() -> Mutex<EmergencyReclaim> {
    Mutex::new(EmergencyReclaim::new())
}

#[test]
fn suppress_decision_drops_the_event() {
    let sink = RecordingSink::new(CaptureDecision::Suppress);
    let key = LocalInputEvent::Key {
        usage: 0xE3,
        down: true,
        mods: 0,
    };
    assert!(matches!(
        decision_for(&sink, key, &reclaim()),
        CallbackResult::Drop
    ));
    assert_eq!(sink.seen.lock().expect("seen").as_slice(), &[key]);
}

#[test]
fn passthrough_decision_keeps_the_event() {
    let sink = RecordingSink::new(CaptureDecision::PassThrough);
    let key = LocalInputEvent::Key {
        usage: 0xE0,
        down: false,
        mods: 0,
    };
    assert!(matches!(
        decision_for(&sink, key, &reclaim()),
        CallbackResult::Keep
    ));
}

#[test]
fn panicking_sink_defaults_to_passthrough() {
    let key = LocalInputEvent::Key {
        usage: 0x04,
        down: true,
        mods: 0,
    };
    assert!(matches!(
        decision_for(&PanickingSink, key, &reclaim()),
        CallbackResult::Keep
    ));
}

#[test]
fn emergency_chord_forces_passthrough_but_reaches_sink() {
    let sink = RecordingSink::new(CaptureDecision::Suppress);
    let reclaim = reclaim();
    let left = LocalInputEvent::Key {
        usage: 0xE0,
        down: true,
        mods: 1 << 0,
    };
    let right = LocalInputEvent::Key {
        usage: 0xE4,
        down: true,
        mods: (1 << 0) | (1 << 4),
    };

    assert!(matches!(
        decision_for(&sink, left, &reclaim),
        CallbackResult::Drop
    ));
    assert!(matches!(
        decision_for(&sink, right, &reclaim),
        CallbackResult::Keep
    ));
    assert_eq!(sink.seen.lock().expect("seen").as_slice(), &[left, right]);
}

#[test]
fn lock_recover_recovers_a_poisoned_mutex() {
    let m = Arc::new(Mutex::new(7_u32));
    let m2 = Arc::clone(&m);
    let _ = std::thread::spawn(move || {
        let _g = m2.lock().expect("acquire to poison");
        panic!("poison the mutex");
    })
    .join();
    assert!(m.lock().is_err(), "mutex should be poisoned");
    assert_eq!(*lock_recover(&m), 7);
}

#[test]
fn capture_control_path_survives_poison() {
    let cap = MacCapture::new();
    let inner = Arc::clone(&cap.inner);
    let _ = std::thread::spawn(move || {
        let _g = inner.lock().expect("acquire to poison");
        panic!("poison the capture mutex");
    })
    .join();
    assert!(
        cap.inner.lock().is_err(),
        "capture mutex should be poisoned"
    );
    assert!(!cap.can_suppress());
    assert!(cap.stop().is_ok());
}

#[test]
fn unknown_display_id_falls_back_to_main() {
    // An unknown/0 display id has no bounds, so `move_cursor` falls back to the main
    // display instead of erroring. Erroring would drop a controlled peer's motion AND
    // trip `on_injection_failed`, which latches input off -> the controlled peer flaps
    // and won't cross. (The source addresses the target's primary display as id 0.)
    assert!(crate::display_info::display_bounds(u32::MAX).is_none());
    assert!(crate::display_info::main_display_bounds().w > 0.0);
}
