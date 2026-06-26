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
fn take_current_invalidates_in_flight_starts() {
    let cap = MacCapture::new();
    let before = lock_recover(&cap.inner).generation;
    let _ = cap.take_current();
    assert_ne!(
        lock_recover(&cap.inner).generation,
        before,
        "detaching capture must invalidate stale start threads"
    );
}

#[test]
fn stale_passive_start_does_not_publish_over_newer_generation() {
    let cap = MacCapture::new();
    let sink: Arc<dyn InputSink> = Arc::new(RecordingSink::new(CaptureDecision::PassThrough));
    let stale_generation = {
        let mut g = lock_recover(&cap.inner);
        let stale = bump_generation(&mut g);
        bump_generation(&mut g);
        stale
    };

    cap.start_passive_poll(sink, stale_generation)
        .expect("stale passive start is ignored without failing");
    let g = lock_recover(&cap.inner);
    assert_eq!(g.mode, CaptureMode::Off);
    assert!(
        g.passive_stop.is_none() && g.passive_handle.is_none(),
        "stale passive start must not publish worker state"
    );
}

#[test]
fn off_invalidates_pending_start() {
    let cap = MacCapture::new();
    let sink: Arc<dyn InputSink> = Arc::new(RecordingSink::new(CaptureDecision::PassThrough));
    let pending_generation = {
        let mut g = lock_recover(&cap.inner);
        let generation = bump_generation(&mut g);
        g.pending_mode = Some(CaptureMode::PassiveEdge);
        generation
    };

    cap.set_mode(CaptureMode::Off, &sink)
        .expect("off transition clears pending start");
    let g = lock_recover(&cap.inner);
    assert_ne!(
        g.generation, pending_generation,
        "Off must invalidate an in-flight start instead of treating Off as a no-op"
    );
    assert_eq!(g.mode, CaptureMode::Off);
    assert_eq!(g.pending_mode, None);
}

#[test]
fn start_failure_clears_matching_pending_mode() {
    let cap = MacCapture::new();
    let pending_generation = {
        let mut g = lock_recover(&cap.inner);
        let generation = bump_generation(&mut g);
        g.pending_mode = Some(CaptureMode::ActiveForward);
        g.mode = CaptureMode::Off;
        g.can_suppress = true;
        generation
    };

    clear_pending_start(&mut lock_recover(&cap.inner), pending_generation);
    let g = lock_recover(&cap.inner);
    assert_eq!(g.pending_mode, None);
    assert_eq!(g.mode, CaptureMode::Off);
    assert!(!g.can_suppress);
}

#[test]
fn stop_detached_does_not_join_current_passive_thread() {
    let stop = Arc::new(AtomicBool::new(false));
    let (handle_tx, handle_rx) = std::sync::mpsc::channel();
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    let worker_stop = Arc::clone(&stop);

    let handle = std::thread::spawn(move || {
        let own_handle = handle_rx.recv().expect("own thread handle");
        stop_detached(None, Some(worker_stop), Some(own_handle));
        done_tx.send(()).expect("done signal");
    });
    handle_tx.send(handle).expect("send own handle");

    assert!(
        done_rx.recv_timeout(Duration::from_secs(1)).is_ok(),
        "current-thread stop must not block trying to join itself"
    );
    assert!(stop.load(Ordering::Acquire));
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
