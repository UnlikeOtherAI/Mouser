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

use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use mouser_core::platform::{
    CaptureDecision as CoreDecision, CaptureMode, InputCapture, InputInjection, InputSink,
    LocalInputEvent,
};
use mouser_net::InteractiveConnection;

use crate::core::{Action, CaptureDecision, EngineCore, Inject};

/// One queued outbound message for the sender task.
enum Outgoing {
    Control(u16, Vec<u8>),
    Motion(mouser_protocol::PointerMotion),
}

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
}

/// Lock the core, recovering the inner value if a task panicked while holding it
/// (panic-free discipline: no `unwrap` on the poisoned guard).
fn lock(core: &Mutex<EngineCore>) -> std::sync::MutexGuard<'_, EngineCore> {
    core.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
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
                Action::Inject(inject) => apply_inject(&self.injector, inject),
                Action::Capture(d) => decision = Some(d),
                // Decoupled: applied by the mode task, never inline (see module docs).
                Action::SetCaptureMode(mode) => {
                    let _ = self.mode_tx.send(mode);
                }
                Action::OwnerChanged { .. } => {}
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
}

fn apply_inject(injector: &Arc<dyn InputInjection>, inject: Inject) {
    let result = match inject {
        Inject::MoveCursor { display_id, x, y } => injector.move_cursor(display_id, x, y),
        Inject::Button { button, down } => injector.button(button, down),
        Inject::Key { usage, down, mods } => injector.key(usage, down, mods),
        Inject::Scroll { dx, dy, unit } => injector.scroll(dx, dy, unit),
    };
    // Injection failures (transient OS error, lost permission) are non-fatal here;
    // capability changes are surfaced separately via CapabilityState (future work).
    let _ = result;
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
        let (mode_tx, mut mode_rx) = tokio::sync::mpsc::unbounded_channel::<CaptureMode>();
        let shared = Arc::new(Shared {
            core: Mutex::new(core),
            out_tx,
            injector,
            capture,
            mode_tx,
        });
        let mut tasks = Vec::new();

        // Sender: serialize all outbound control/motion onto the connection.
        {
            let conn = Arc::clone(&conn);
            tasks.push(tokio::spawn(async move {
                while let Some(msg) = out_rx.recv().await {
                    match msg {
                        Outgoing::Control(ty, payload) => {
                            if conn.send_control(ty, &payload).await.is_err() {
                                break;
                            }
                        }
                        Outgoing::Motion(motion) => {
                            let _ = conn.send_motion(&motion);
                        }
                    }
                }
            }));
        }

        // Control receiver.
        {
            let conn = Arc::clone(&conn);
            let shared = Arc::clone(&shared);
            tasks.push(tokio::spawn(async move {
                while let Ok((ty, payload)) = conn.recv_control().await {
                    let actions = lock(&shared.core).on_control(ty, &payload);
                    shared.dispatch(actions);
                }
            }));
        }

        // Motion receiver.
        {
            let conn = Arc::clone(&conn);
            let shared = Arc::clone(&shared);
            tasks.push(tokio::spawn(async move {
                while let Ok(datagram) = conn.recv_motion().await {
                    let actions = lock(&shared.core).on_motion(datagram);
                    shared.dispatch(actions);
                }
            }));
        }

        // Heartbeat ticker.
        {
            let shared = Arc::clone(&shared);
            tasks.push(tokio::spawn(async move {
                let mut interval = tokio::time::interval(tick);
                interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                loop {
                    interval.tick().await;
                    let actions = lock(&shared.core).on_tick();
                    shared.dispatch(actions);
                }
            }));
        }

        // Capture-mode task: apply SetCaptureMode off the capture threads so a
        // transition triggered from inside the sink can't deadlock by joining its
        // own poll/hook thread (see module docs).
        {
            let shared = Arc::clone(&shared);
            tasks.push(tokio::spawn(async move {
                let sink = shared.make_sink();
                while let Some(mode) = mode_rx.recv().await {
                    if let Err(e) = shared.capture.set_mode(mode, &sink) {
                        eprintln!("mouserd: capture set_mode({mode:?}) failed: {e}");
                    }
                }
            }));
        }

        // Bring capture up in the correct initial mode (passive edge sensing for a
        // source; off for a target). Buffered on the channel until the mode task runs.
        let initial = lock(&shared.core).initial_actions();
        shared.dispatch(initial);

        Self { shared, tasks }
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

    /// Stop all background tasks and tear capture down.
    pub fn shutdown(self) {
        let Self { shared, tasks } = self;
        for task in tasks {
            task.abort();
        }
        // Tasks are aborted first so no further set_mode can race; then a clean
        // teardown of any installed hooks/poll thread.
        let _ = shared.capture.stop();
    }
}
