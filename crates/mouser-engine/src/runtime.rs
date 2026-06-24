//! Async runtime that drives [`EngineCore`] over a live [`InteractiveConnection`],
//! a platform [`InputInjection`] adapter, and a platform [`InputCapture`] adapter.
//!
//! The core is sans-IO; this module is the thin IO shell. It spawns:
//! - a **control receiver** that decodes frames and feeds [`EngineCore::on_control`],
//! - a **motion receiver** that feeds [`EngineCore::on_motion`],
//! - a **heartbeat ticker** that calls [`EngineCore::on_tick`] every interval,
//! - a single **sender** task that serializes all outbound control/motion,
//! - a **capture-mode task** that applies [`Action::SetCaptureMode`] by calling
//!   [`InputCapture::set_mode`].
//!
//! ## Capture is ownership-driven, not connection-driven
//! The runtime never installs an always-on input hook. On start it applies
//! [`EngineCore::initial_actions`] (a source comes up in
//! [`CaptureMode::PassiveEdge`]; a target in [`CaptureMode::Off`]). Thereafter the
//! core emits [`Action::SetCaptureMode`] on every ownership change, so heavyweight
//! suppressing capture ([`CaptureMode::ActiveForward`]) exists **only** while this
//! machine is actively driving a remote peer.
//!
//! ## Why the mode task is decoupled from the capture threads
//! The capture adapter's sink calls back into [`feed_local`](RuntimeHandle::feed_local),
//! which can itself produce an [`Action::SetCaptureMode`] (e.g. a passive edge
//! crossing escalates to `ActiveForward`). If we applied that switch inline we would
//! be asking the capture's own poll/hook thread to stop and join *itself* —
//! deadlock. Instead every `SetCaptureMode` is forwarded over a channel to a
//! dedicated task that runs on the tokio pool, so `set_mode` (which stops/joins the
//! previous mode's threads) never runs on a thread it is trying to join.
//!
//! The sink handed to the capture adapter holds only a [`std::sync::Weak`] back to
//! the runtime's [`Shared`] state, so the adapter storing the sink never forms a
//! strong reference cycle with the runtime.

mod control_lane;
mod liveness;

use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use mouser_core::platform::{
    CaptureDecision as CoreDecision, CaptureMode, InputCapture, InputInjection, InputSink,
    LocalInputEvent,
};
use mouser_core::DeviceId;
use mouser_net::{InteractiveConnection, MotionPlane};
use mouser_protocol::{from_cbor, to_cbor, Datagram, PointerMotion};
use tokio_util::sync::CancellationToken;

use crate::core::{Action, CaptureDecision, EngineCore, Inject};
use control_lane::{is_side_control, ControlLane, ControlMessage, Outgoing};
use liveness::DeathState;

pub use control_lane::ControlLane as RuntimeControlLane;

/// Engine-private control-stream fallback for absolute pointer motion when QUIC
/// DATAGRAM is unavailable. Kept out of `mouser-protocol` until the wire registry is
/// formalized; unknown peers skip it as a forward-compatible control type.
const TYPE_POINTER_MOTION_FALLBACK: u16 = 0x0043;

/// State shared by the runtime's background tasks and the capture sink.
///
/// Holds the core state machine plus the seams the dispatched [`Action`]s need:
/// outbound queue, injector, capture adapter, and the capture-mode channel.
struct Shared {
    core: Mutex<EngineCore>,
    out_tx: tokio::sync::mpsc::UnboundedSender<Outgoing>,
    injector: Arc<dyn InputInjection>,
    capture: Arc<dyn InputCapture>,
    /// `Action::SetCaptureMode` is forwarded here and applied off the capture
    /// threads by the mode task (see module docs).
    mode_tx: tokio::sync::mpsc::UnboundedSender<CaptureMode>,
    /// `Action::OwnerChanged` is forwarded here so an embedder (the daemon's IPC
    /// bridge) can reflect the live owner/epoch in its snapshot. Without this the
    /// snapshot keeps the owner/epoch it had at connect time and never tracks a
    /// cross — on either side (it's emitted both when we cross out and when we
    /// receive a peer's ownership transfer).
    owner_tx: tokio::sync::mpsc::UnboundedSender<(DeviceId, u64)>,
}

/// Runtime liveness surfaced to embedders and tests.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RuntimeState {
    Running,
    Dead,
}

/// Lock shared state, recovering the inner value if a task panicked while holding it
/// (panic-free discipline: no `unwrap` on the poisoned guard).
fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

impl Shared {
    /// Apply the non-IO-deferred actions: queue outbound frames/motion, inject input,
    /// and forward capture-mode transitions to the mode task. Returns the per-event
    /// capture decision if one was present (used on the local-input path so mac/linux
    /// adapters can suppress the very event the core rejected).
    fn dispatch(&self, actions: Vec<Action>) -> Option<CaptureDecision> {
        let mut decision = None;
        for action in actions {
            match action {
                Action::SendControl(ty, payload) => {
                    let _ = self.out_tx.send(Outgoing::Control(ty, payload));
                }
                Action::SendMotion(motion) => {
                    let _ = self.out_tx.send(Outgoing::Motion(motion));
                }
                Action::Inject(inject) => {
                    if let Err(e) = apply_inject(&self.injector, inject) {
                        crate::diag!(info,
                            "mouserd: input injection failed; blocking remote input and returning ownership: {e}"
                        );
                        let actions = lock(&self.core).on_injection_failed();
                        self.dispatch(actions);
                    }
                }
                Action::SetCursorVisible(visible) => {
                    if let Err(e) = self.injector.set_cursor_visible(visible) {
                        crate::diag!(warn, "mouserd: cursor visibility update failed: {e}");
                    }
                }
                Action::Capture(d) => decision = Some(d),
                // Decoupled: applied by the mode task, never inline (see module docs).
                Action::SetCaptureMode(mode) => {
                    let _ = self.mode_tx.send(mode);
                }
                Action::OwnerChanged { owner, epoch } => {
                    let _ = self.owner_tx.send((owner, epoch));
                }
            }
        }
        decision
    }

    /// Run one captured local event through the core and apply the resulting actions.
    fn feed_local(&self, event: LocalInputEvent) -> CaptureDecision {
        let actions = lock(&self.core).on_local_input(event);
        self.dispatch(actions)
            .unwrap_or(CaptureDecision::PassThrough)
    }

    /// Build a fresh capture sink that holds only a [`Weak`] back to this state, so
    /// the capture adapter storing the sink can never keep the runtime alive.
    fn make_sink(self: &Arc<Self>) -> Arc<dyn InputSink> {
        Arc::new(CaptureSink {
            shared: Arc::downgrade(self),
        })
    }
}

/// The [`InputSink`] handed to the capture adapter. Holds a [`Weak`] so the adapter
/// (which stores the sink while capturing) never forms a strong cycle with the
/// runtime; once the runtime is gone, captured events harmlessly pass through.
struct CaptureSink {
    shared: Weak<Shared>,
}

impl InputSink for CaptureSink {
    fn on_event(&self, event: LocalInputEvent) -> CoreDecision {
        let Some(shared) = self.shared.upgrade() else {
            return CoreDecision::PassThrough;
        };
        match shared.feed_local(event) {
            CaptureDecision::Suppress => CoreDecision::Suppress,
            CaptureDecision::PassThrough => CoreDecision::PassThrough,
        }
    }
}

/// A running engine: background tasks plus the shared state seam.
pub struct RuntimeHandle {
    shared: Arc<Shared>,
    tasks: Vec<tokio::task::JoinHandle<()>>,
    cancel: CancellationToken,
    death: DeathState,
    control_lane: Option<ControlLane>,
    /// Live `(owner, epoch)` updates emitted on every ownership change; claimed once
    /// by the daemon session so it can keep the IPC snapshot's owner/epoch current.
    owner_rx: Option<tokio::sync::mpsc::UnboundedReceiver<(DeviceId, u64)>>,
}

fn apply_inject(
    injector: &Arc<dyn InputInjection>,
    inject: Inject,
) -> mouser_core::platform::PlatformResult<()> {
    match inject {
        Inject::MoveCursor { display_id, x, y } => injector.move_cursor(display_id, x, y),
        Inject::Button { button, down } => injector.button(button, down),
        Inject::Key { usage, down, mods } => injector.key(usage, down, mods),
        Inject::Scroll { dx, dy, unit } => injector.scroll(dx, dy, unit),
    }
}

fn fallback_motion(payload: &[u8]) -> Option<Datagram> {
    from_cbor::<PointerMotion>(payload)
        .ok()
        .map(Datagram::Motion)
}

impl RuntimeHandle {
    /// Start the engine over `conn`, applying received input via `injector` and
    /// driving local capture via `capture`. Capture comes up in the core's initial
    /// mode (passive edge sensing for a source, off for a target).
    pub fn start(
        core: EngineCore,
        conn: Arc<InteractiveConnection>,
        injector: Arc<dyn InputInjection>,
        capture: Arc<dyn InputCapture>,
    ) -> Self {
        Self::start_with_interval(core, conn, injector, capture, Duration::from_secs(1))
    }

    /// Like [`start`](Self::start) but with a configurable heartbeat interval (tests).
    pub fn start_with_interval(
        core: EngineCore,
        conn: Arc<InteractiveConnection>,
        injector: Arc<dyn InputInjection>,
        capture: Arc<dyn InputCapture>,
        tick: Duration,
    ) -> Self {
        let (out_tx, mut out_rx) = tokio::sync::mpsc::unbounded_channel::<Outgoing>();
        let (side_tx, side_rx) = tokio::sync::mpsc::unbounded_channel::<ControlMessage>();
        let (mode_tx, mut mode_rx) = tokio::sync::mpsc::unbounded_channel::<CaptureMode>();
        let (owner_tx, owner_rx) = tokio::sync::mpsc::unbounded_channel::<(DeviceId, u64)>();
        let cancel = CancellationToken::new();
        let death = DeathState::new();
        let shared = Arc::new(Shared {
            core: Mutex::new(core),
            out_tx,
            injector,
            capture,
            mode_tx,
            owner_tx,
        });
        let mut tasks = Vec::new();

        // Sender: serialize all outbound control/motion onto the connection.
        {
            let conn = Arc::clone(&conn);
            let cancel = cancel.clone();
            let death = death.clone();
            let motion_plane = conn.motion_plane();
            tasks.push(tokio::spawn(async move {
                let motion_plane = motion_plane;
                loop {
                    tokio::select! {
                        _ = cancel.cancelled() => break,
                        msg = out_rx.recv() => {
                            let Some(msg) = msg else {
                                death.mark(&cancel, &conn, "runtime outbound queue closed");
                                break;
                            };
                            match msg {
                                Outgoing::Control(ty, payload) => {
                                    if let Err(e) = conn.send_control(ty, &payload).await {
                                        death.mark(&cancel, &conn, format!("control send failed: {e}"));
                                        break;
                                    }
                                }
                                Outgoing::Motion(motion) => {
                                    let send = if *motion_plane.borrow() == MotionPlane::ControlFallback {
                                        match to_cbor(&motion) {
                                            Ok(payload) => conn
                                                .send_control(TYPE_POINTER_MOTION_FALLBACK, &payload)
                                                .await,
                                            Err(e) => {
                                                crate::diag!(info,
                                                    "mouserd: failed to encode fallback pointer motion: {e}"
                                                );
                                                Ok(())
                                            }
                                        }
                                    } else {
                                        conn.send_motion(&motion)
                                    };
                                    if let Err(e) = send {
                                        death.mark(&cancel, &conn, format!("motion send failed: {e}"));
                                        break;
                                    }
                                }
                            }
                        }
                    }
                }
            }));
        }

        // Control receiver.
        {
            let conn = Arc::clone(&conn);
            let shared = Arc::clone(&shared);
            let cancel = cancel.clone();
            let death = death.clone();
            tasks.push(tokio::spawn(async move {
                loop {
                    tokio::select! {
                        _ = cancel.cancelled() => break,
                        msg = conn.recv_control() => match msg {
                            Ok((TYPE_POINTER_MOTION_FALLBACK, payload)) => {
                                if let Some(datagram) = fallback_motion(&payload) {
                                    let actions = lock(&shared.core).on_motion(datagram);
                                    shared.dispatch(actions);
                                }
                            }
                            Ok((ty, payload)) if is_side_control(ty) => {
                                let _ = side_tx.send(ControlMessage { ty, payload });
                            }
                            Ok((ty, payload)) => {
                                let actions = lock(&shared.core).on_control(ty, &payload);
                                shared.dispatch(actions);
                            }
                            Err(e) => {
                                death.mark(&cancel, &conn, format!("control receive failed: {e}"));
                                break;
                            }
                        }
                    }
                }
            }));
        }

        // Motion receiver.
        {
            let conn = Arc::clone(&conn);
            let shared = Arc::clone(&shared);
            let cancel = cancel.clone();
            let death = death.clone();
            tasks.push(tokio::spawn(async move {
                loop {
                    tokio::select! {
                        _ = cancel.cancelled() => break,
                        datagram = conn.recv_motion() => match datagram {
                            Ok(datagram) => {
                                let actions = lock(&shared.core).on_motion(datagram);
                                shared.dispatch(actions);
                            }
                            Err(e) => {
                                death.mark(&cancel, &conn, format!("motion receive failed: {e}"));
                                break;
                            }
                        }
                    }
                }
            }));
        }

        // Heartbeat ticker.
        {
            let shared = Arc::clone(&shared);
            let cancel = cancel.clone();
            tasks.push(tokio::spawn(async move {
                let mut interval = tokio::time::interval(tick);
                interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                loop {
                    tokio::select! {
                        _ = cancel.cancelled() => break,
                        _ = interval.tick() => {
                            let actions = lock(&shared.core).on_tick();
                            shared.dispatch(actions);
                        }
                    }
                }
            }));
        }

        // Capture-mode task: apply SetCaptureMode off the capture threads so a
        // transition triggered from inside the sink can't deadlock by joining its
        // own poll/hook thread (see module docs).
        {
            let shared = Arc::clone(&shared);
            let cancel = cancel.clone();
            tasks.push(tokio::spawn(async move {
                let sink = shared.make_sink();
                loop {
                    tokio::select! {
                        _ = cancel.cancelled() => break,
                        mode = mode_rx.recv() => {
                            let Some(mode) = mode else { break };
                            match shared.capture.set_mode(mode, &sink) {
                                Ok(()) if mode == CaptureMode::ActiveForward
                                    && !shared.capture.can_suppress() =>
                                {
                                    // Fail closed: we can't drop local input, so don't keep
                                    // forwarding (that would drive BOTH machines). Reclaim
                                    // local control; the UI prompts to grant Accessibility +
                                    // Input Monitoring.
                                    crate::diag!(warn,
                                        "mouserd: cannot suppress local input (grant Accessibility + Input Monitoring) — reclaiming local control instead of driving both machines"
                                    );
                                    let actions = lock(&shared.core).on_suppress_unavailable();
                                    shared.dispatch(actions);
                                }
                                Ok(()) => {}
                                Err(e) => {
                                    crate::diag!(warn, "mouserd: capture set_mode({mode:?}) failed: {e}");
                                    if mode == CaptureMode::ActiveForward {
                                        let actions = lock(&shared.core).on_suppress_unavailable();
                                        shared.dispatch(actions);
                                    }
                                }
                            }
                        }
                    }
                }
            }));
        }

        // Bring capture up in the correct initial mode (passive edge sensing for a
        // source; off for a target). Buffered on the channel until the mode task runs.
        let initial = lock(&shared.core).initial_actions();
        shared.dispatch(initial);

        let control_lane = ControlLane::new(shared.out_tx.clone(), side_rx);
        Self {
            shared,
            tasks,
            cancel,
            death,
            control_lane: Some(control_lane),
            owner_rx: Some(owner_rx),
        }
    }

    /// Take the daemon side control lane, if it has not already been claimed.
    pub fn take_control_lane(&mut self) -> Option<RuntimeControlLane> {
        self.control_lane.take()
    }

    /// Take the live `(owner, epoch)` update stream, if not already claimed. The daemon
    /// session forwards these to the IPC bridge so the UI snapshot reflects the real
    /// owner/epoch through every cross (see `Action::OwnerChanged`).
    pub fn take_owner_updates(
        &mut self,
    ) -> Option<tokio::sync::mpsc::UnboundedReceiver<(DeviceId, u64)>> {
        self.owner_rx.take()
    }

    /// Feed one captured local event through the core (the `InputSink` seam used by
    /// tests and the in-process direct path). Returns the [`CaptureDecision`] the
    /// adapter should apply (defaults to pass-through).
    pub fn feed_local(&self, event: LocalInputEvent) -> CaptureDecision {
        self.shared.feed_local(event)
    }

    /// Whether this node currently owns input.
    pub fn is_owner(&self) -> bool {
        lock(&self.shared.core).is_owner()
    }

    /// Current ownership epoch.
    pub fn epoch(&self) -> u64 {
        lock(&self.shared.core).epoch()
    }

    /// The capture mode the adapter is currently in (observability/tests).
    pub fn capture_mode(&self) -> CaptureMode {
        self.shared.capture.current_mode()
    }

    /// Runtime liveness: `Dead` means a network task observed connection loss or the
    /// outbound sender ended, and the sibling tasks have been cancelled.
    pub fn state(&self) -> RuntimeState {
        if self.death.is_dead() {
            RuntimeState::Dead
        } else {
            RuntimeState::Running
        }
    }

    /// Whether the runtime has reached a terminal connection-dead state.
    pub fn is_dead(&self) -> bool {
        self.state() == RuntimeState::Dead
    }

    pub fn death_reason(&self) -> Option<String> {
        self.death.reason()
    }

    /// Resolve when the runtime reaches the terminal connection-dead state.
    pub async fn wait_dead(&self) {
        self.death.wait().await;
    }

    /// Stop all background tasks and tear capture down.
    pub fn shutdown(self) {
        let Self {
            shared,
            tasks,
            cancel,
            death: _,
            control_lane: _,
            owner_rx: _,
        } = self;
        cancel.cancel();
        for task in tasks {
            task.abort();
        }
        let _ = shared.injector.set_cursor_visible(true);
        let _ = shared.capture.stop();
    }
}
