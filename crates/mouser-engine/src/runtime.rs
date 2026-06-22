//! Async runtime that drives [`EngineCore`] over a live [`InteractiveConnection`]
//! and a platform [`InputInjection`] adapter.
//!
//! The core is sans-IO; this module is the thin IO shell. It spawns:
//! - a **control receiver** that decodes frames and feeds [`EngineCore::on_control`],
//! - a **motion receiver** that feeds [`EngineCore::on_motion`],
//! - a **heartbeat ticker** that calls [`EngineCore::on_tick`] every interval,
//! - a single **sender** task that serializes all outbound control/motion.
//!
//! [`RuntimeHandle::feed_local`] is the seam a capture adapter's `InputSink` calls for
//! each local event; it runs the core synchronously and returns the
//! [`CaptureDecision`] so the adapter can suppress/pass-through.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use mouser_core::platform::InputInjection;
use mouser_core::platform::LocalInputEvent;
use mouser_net::InteractiveConnection;

use crate::core::{Action, CaptureDecision, EngineCore, Inject};

/// One queued outbound message for the sender task.
enum Outgoing {
    Control(u16, Vec<u8>),
    Motion(mouser_protocol::PointerMotion),
}

/// A running engine: background tasks plus the seam to feed local input.
pub struct RuntimeHandle {
    core: Arc<Mutex<EngineCore>>,
    out_tx: tokio::sync::mpsc::UnboundedSender<Outgoing>,
    injector: Arc<dyn InputInjection>,
    tasks: Vec<tokio::task::JoinHandle<()>>,
}

/// Lock the core, recovering the inner value if a task panicked while holding it
/// (panic-free discipline: no `unwrap` on the poisoned guard).
fn lock(core: &Mutex<EngineCore>) -> std::sync::MutexGuard<'_, EngineCore> {
    core.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Apply the non-IO-deferred actions: queue outbound frames/motion and inject input.
/// Returns the capture decision if one was present (used on the local-input path).
fn dispatch(
    actions: Vec<Action>,
    out_tx: &tokio::sync::mpsc::UnboundedSender<Outgoing>,
    injector: &Arc<dyn InputInjection>,
) -> Option<CaptureDecision> {
    let mut decision = None;
    for action in actions {
        match action {
            Action::SendControl(ty, payload) => {
                let _ = out_tx.send(Outgoing::Control(ty, payload));
            }
            Action::SendMotion(motion) => {
                let _ = out_tx.send(Outgoing::Motion(motion));
            }
            Action::Inject(inject) => apply_inject(injector, inject),
            Action::Capture(d) => decision = Some(d),
            Action::OwnerChanged { .. } => {}
        }
    }
    decision
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
    /// Start the engine over `conn`, applying received input via `injector`.
    pub fn start(
        core: EngineCore,
        conn: Arc<InteractiveConnection>,
        injector: Arc<dyn InputInjection>,
    ) -> Self {
        Self::start_with_interval(core, conn, injector, Duration::from_secs(1))
    }

    /// Like [`start`](Self::start) but with a configurable heartbeat interval (tests).
    pub fn start_with_interval(
        core: EngineCore,
        conn: Arc<InteractiveConnection>,
        injector: Arc<dyn InputInjection>,
        tick: Duration,
    ) -> Self {
        let core = Arc::new(Mutex::new(core));
        let (out_tx, mut out_rx) = tokio::sync::mpsc::unbounded_channel::<Outgoing>();
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
            let core = Arc::clone(&core);
            let out_tx = out_tx.clone();
            let injector = Arc::clone(&injector);
            tasks.push(tokio::spawn(async move {
                while let Ok((ty, payload)) = conn.recv_control().await {
                    let actions = lock(&core).on_control(ty, &payload);
                    dispatch(actions, &out_tx, &injector);
                }
            }));
        }

        // Motion receiver.
        {
            let conn = Arc::clone(&conn);
            let core = Arc::clone(&core);
            let out_tx = out_tx.clone();
            let injector = Arc::clone(&injector);
            tasks.push(tokio::spawn(async move {
                while let Ok(datagram) = conn.recv_motion().await {
                    let actions = lock(&core).on_motion(datagram);
                    dispatch(actions, &out_tx, &injector);
                }
            }));
        }

        // Heartbeat ticker.
        {
            let core = Arc::clone(&core);
            let out_tx = out_tx.clone();
            let injector = Arc::clone(&injector);
            tasks.push(tokio::spawn(async move {
                let mut interval = tokio::time::interval(tick);
                interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                loop {
                    interval.tick().await;
                    let actions = lock(&core).on_tick();
                    dispatch(actions, &out_tx, &injector);
                }
            }));
        }

        Self { core, out_tx, injector, tasks }
    }

    /// Feed one captured local event through the core (the `InputSink` seam). Returns
    /// the [`CaptureDecision`] the adapter should apply (defaults to pass-through).
    pub fn feed_local(&self, event: LocalInputEvent) -> CaptureDecision {
        let actions = lock(&self.core).on_local_input(event);
        dispatch(actions, &self.out_tx, &self.injector).unwrap_or(CaptureDecision::PassThrough)
    }

    /// Whether this node currently owns input.
    pub fn is_owner(&self) -> bool {
        lock(&self.core).is_owner()
    }

    /// Current ownership epoch.
    pub fn epoch(&self) -> u64 {
        lock(&self.core).epoch()
    }

    /// Stop all background tasks.
    pub fn shutdown(self) {
        for task in self.tasks {
            task.abort();
        }
    }
}
