use super::*;

use mouser_core::platform::{CaptureDecision, InputSink};
use std::sync::Arc;

#[derive(Default)]
struct RecordingSink;

impl InputSink for RecordingSink {
    fn on_event(&self, _event: LocalInputEvent) -> CaptureDecision {
        CaptureDecision::PassThrough
    }
}

#[test]
fn new_capture_is_off_and_cannot_suppress() {
    let cap = WinCapture::new();
    assert_eq!(cap.current_mode(), CaptureMode::Off);
    assert!(!cap.can_suppress());
}

#[test]
fn passive_mode_installs_no_hooks() {
    let _guard = crate::capture_hooks::capture_test_lock();
    let cap = WinCapture::new();
    let sink: Arc<dyn InputSink> = Arc::new(RecordingSink);

    cap.set_mode(CaptureMode::PassiveEdge, &sink)
        .expect("enter passive edge");
    assert_eq!(cap.current_mode(), CaptureMode::PassiveEdge);
    assert!(
        !cap.can_suppress(),
        "passive edge sensing never suppresses local input"
    );
    {
        let run = lock_recover(&cap.inner);
        assert!(
            run.hook_thread_id.is_none(),
            "no WH_*_LL hooks are installed in passive mode"
        );
        assert!(run.passive_handle.is_some(), "the poll thread is running");
    }

    cap.set_mode(CaptureMode::PassiveEdge, &sink)
        .expect("idempotent");
    assert_eq!(cap.current_mode(), CaptureMode::PassiveEdge);

    cap.stop().expect("stop");
    assert_eq!(cap.current_mode(), CaptureMode::Off);
    {
        let run = lock_recover(&cap.inner);
        assert!(run.passive_handle.is_none(), "poll thread joined on stop");
    }
}
