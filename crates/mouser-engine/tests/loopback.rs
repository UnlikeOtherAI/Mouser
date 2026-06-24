//! End-to-end: a source engine and a target engine connected by a real QUIC loopback
//! connection. The source feeds synthetic local input; the target injects what it
//! receives into a recording fake adapter (no real cursor is touched). Proves the
//! whole pipeline — capture → ownership handoff → forward over QUIC → inject — works
//! over the actual transport, not just in the pure core.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use mouser_core::platform::{
    CaptureMode, InputCapture, InputInjection, InputSink, LocalInputEvent, PlatformResult,
    ScrollUnit,
};
use mouser_engine::{EdgeLayout, EngineCore, RuntimeHandle};
use mouser_net::{DeviceIdentity, InteractiveEndpoint, PinPolicy};

#[derive(Debug, PartialEq, Eq)]
enum Recorded {
    Move { x: i32, y: i32 },
    Button { button: u8, down: bool },
    Key { usage: u16, down: bool },
    Scroll { dx: i32, dy: i32 },
}

/// Records every injection instead of touching the OS.
struct RecordingInjector {
    tx: tokio::sync::mpsc::UnboundedSender<Recorded>,
}

impl InputInjection for RecordingInjector {
    fn move_cursor(&self, _display_id: u32, x: i32, y: i32) -> PlatformResult<()> {
        let _ = self.tx.send(Recorded::Move { x, y });
        Ok(())
    }
    fn move_cursor_relative(&self, _dx: i32, _dy: i32) -> PlatformResult<()> {
        Ok(())
    }
    fn button(&self, button: u8, down: bool) -> PlatformResult<()> {
        let _ = self.tx.send(Recorded::Button { button, down });
        Ok(())
    }
    fn key(&self, usage: u16, down: bool, _mods: u16) -> PlatformResult<()> {
        let _ = self.tx.send(Recorded::Key { usage, down });
        Ok(())
    }
    fn scroll(&self, dx: i32, dy: i32, _unit: ScrollUnit) -> PlatformResult<()> {
        let _ = self.tx.send(Recorded::Scroll { dx, dy });
        Ok(())
    }
}

/// A capture adapter that records every `set_mode` call, so a test can prove that
/// an idle/connected source never escalates to `ActiveForward` and only does so on
/// an actual edge crossing. Installs no real hooks.
#[derive(Default)]
struct SpyCapture {
    history: Mutex<Vec<CaptureMode>>,
}

impl SpyCapture {
    fn history(&self) -> Vec<CaptureMode> {
        self.history
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    fn current(&self) -> CaptureMode {
        self.history().last().copied().unwrap_or(CaptureMode::Off)
    }
}

impl InputCapture for SpyCapture {
    fn set_mode(&self, mode: CaptureMode, _sink: &Arc<dyn InputSink>) -> PlatformResult<()> {
        self.history
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(mode);
        Ok(())
    }
    fn stop(&self) -> PlatformResult<()> {
        Ok(())
    }
    fn can_suppress(&self) -> bool {
        matches!(self.current(), CaptureMode::ActiveForward)
    }
    fn current_mode(&self) -> CaptureMode {
        self.current()
    }
}

/// A capture adapter that does nothing (target side / flows that don't assert on it).
struct NoopCapture;
impl InputCapture for NoopCapture {
    fn set_mode(&self, _mode: CaptureMode, _sink: &Arc<dyn InputSink>) -> PlatformResult<()> {
        Ok(())
    }
    fn stop(&self) -> PlatformResult<()> {
        Ok(())
    }
    fn can_suppress(&self) -> bool {
        false
    }
    fn current_mode(&self) -> CaptureMode {
        CaptureMode::Off
    }
}

/// The source side injects nothing in this flow.
struct NoopInjector;
impl InputInjection for NoopInjector {
    fn move_cursor(&self, _d: u32, _x: i32, _y: i32) -> PlatformResult<()> {
        Ok(())
    }
    fn move_cursor_relative(&self, _dx: i32, _dy: i32) -> PlatformResult<()> {
        Ok(())
    }
    fn button(&self, _b: u8, _down: bool) -> PlatformResult<()> {
        Ok(())
    }
    fn key(&self, _u: u16, _down: bool, _m: u16) -> PlatformResult<()> {
        Ok(())
    }
    fn scroll(&self, _dx: i32, _dy: i32, _u: ScrollUnit) -> PlatformResult<()> {
        Ok(())
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn source_drives_target_over_quic() {
    let source_id = DeviceIdentity::generate();
    let target_id = DeviceIdentity::generate();
    let source_device = source_id.device_id();
    let target_device = target_id.device_id();

    // Target listens; source dials. Mutual device_id pinning (§3).
    let server = InteractiveEndpoint::bind_server(
        &target_id,
        mouser_net::loopback_addr(),
        PinPolicy::Pinned(source_device),
    )
    .expect("bind server");
    let server_addr = server.local_addr().expect("server addr");
    let client =
        InteractiveEndpoint::bind_client(mouser_net::loopback_addr()).expect("bind client");

    let accept = tokio::spawn(async move {
        let conn = server.accept_interactive().await.expect("accept");
        (server, conn)
    });
    let source_conn = client
        .connect_interactive(&source_id, server_addr, PinPolicy::Pinned(target_device))
        .await
        .expect("source connect");
    let (_server_ep, target_conn) = accept.await.expect("accept task");

    // Recording injector on the target.
    let (rec_tx, mut rec_rx) = tokio::sync::mpsc::unbounded_channel::<Recorded>();
    let target_rt = RuntimeHandle::start(
        EngineCore::new_target(target_device, source_device),
        Arc::new(target_conn),
        Arc::new(RecordingInjector { tx: rec_tx }),
        Arc::new(NoopCapture),
    );
    let source_rt = RuntimeHandle::start(
        EngineCore::new_source(
            source_device,
            target_device,
            EdgeLayout::side_by_side(100, 100, 100, 100),
        ),
        Arc::new(source_conn),
        Arc::new(NoopInjector),
        Arc::new(NoopCapture),
    );

    // 1. Cross the right edge → source grants ownership to the target.
    source_rt.feed_local(LocalInputEvent::CursorMoved {
        display_id: 0,
        x: 99,
        y: 40,
        dx: 0,
        dy: 0,
    });
    assert!(!source_rt.is_owner(), "source handed input to the target");

    // 2. Forward a key press; the target should inject it once it owns input.
    source_rt.feed_local(LocalInputEvent::Key {
        usage: 0x04,
        down: true,
        mods: 0,
    });

    // The forwarded key must be injected on the target over the real connection.
    let mut got_key = false;
    let mut got_move = false;
    loop {
        match tokio::time::timeout(Duration::from_secs(5), rec_rx.recv()).await {
            Ok(Some(Recorded::Key {
                usage: 0x04,
                down: true,
            })) => {
                got_key = true;
                break;
            }
            Ok(Some(Recorded::Move { .. })) => got_move = true,
            Ok(Some(_)) => {}
            Ok(None) => break,
            Err(_) => break, // timed out
        }
    }

    assert!(
        got_key,
        "the forwarded key was injected on the target over QUIC"
    );
    let _ = got_move; // motion rides the lossy datagram plane; not asserted

    source_rt.shutdown();
    target_rt.shutdown();
}

/// Wait (bounded) for `spy` to reach `want`, yielding so the runtime's capture-mode
/// task can run. Panics with the observed history if it never does.
async fn await_mode(spy: &SpyCapture, want: CaptureMode) {
    for _ in 0..200 {
        if spy.current() == want {
            return;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert_eq!(
        spy.current(),
        want,
        "capture mode never reached target; history = {:?}",
        spy.history()
    );
}

/// The end-to-end proof of "edge sensing is not input forwarding": a connected
/// source sits in passive edge sensing and only escalates to suppressing capture
/// (`ActiveForward`) when the cursor actually crosses the edge — never merely
/// because a connection/IPC link was formed.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn source_capture_escalates_only_on_edge_cross() {
    let source_id = DeviceIdentity::generate();
    let target_id = DeviceIdentity::generate();
    let source_device = source_id.device_id();
    let target_device = target_id.device_id();

    let server = InteractiveEndpoint::bind_server(
        &target_id,
        mouser_net::loopback_addr(),
        PinPolicy::Pinned(source_device),
    )
    .expect("bind server");
    let server_addr = server.local_addr().expect("server addr");
    let client =
        InteractiveEndpoint::bind_client(mouser_net::loopback_addr()).expect("bind client");

    let accept = tokio::spawn(async move {
        let conn = server.accept_interactive().await.expect("accept");
        (server, conn)
    });
    let source_conn = client
        .connect_interactive(&source_id, server_addr, PinPolicy::Pinned(target_device))
        .await
        .expect("source connect");
    let (_server_ep, target_conn) = accept.await.expect("accept task");

    let spy = Arc::new(SpyCapture::default());
    let source_rt = RuntimeHandle::start(
        EngineCore::new_source(
            source_device,
            target_device,
            EdgeLayout::side_by_side(100, 100, 100, 100),
        ),
        Arc::new(source_conn),
        Arc::new(NoopInjector),
        spy.clone(),
    );
    let target_rt = RuntimeHandle::start(
        EngineCore::new_target(target_device, source_device),
        Arc::new(target_conn),
        Arc::new(NoopInjector),
        Arc::new(NoopCapture),
    );

    // On connect, the source comes up in passive edge sensing — never ActiveForward.
    await_mode(&spy, CaptureMode::PassiveEdge).await;
    assert!(
        !spy.history().contains(&CaptureMode::ActiveForward),
        "a freshly connected source must not install forwarding capture; history = {:?}",
        spy.history()
    );

    // Cursor moving on its own screen keeps it passive (no escalation).
    source_rt.feed_local(LocalInputEvent::CursorMoved {
        display_id: 0,
        x: 50,
        y: 50,
        dx: 0,
        dy: 0,
    });
    tokio::time::sleep(Duration::from_millis(40)).await;
    assert!(
        !spy.history().contains(&CaptureMode::ActiveForward),
        "idle cursor motion must not escalate capture; history = {:?}",
        spy.history()
    );

    // Crossing the edge hands ownership to the peer → escalate to ActiveForward.
    source_rt.feed_local(LocalInputEvent::CursorMoved {
        display_id: 0,
        x: 99,
        y: 40,
        dx: 0,
        dy: 0,
    });
    await_mode(&spy, CaptureMode::ActiveForward).await;

    source_rt.shutdown();
    target_rt.shutdown();
}
