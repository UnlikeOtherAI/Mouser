use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use mouser_net::InteractiveConnection;
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

#[derive(Clone)]
pub(super) struct DeathState {
    dead: Arc<AtomicBool>,
    reason: Arc<Mutex<Option<String>>>,
    notify: Arc<Notify>,
}

impl DeathState {
    pub(super) fn new() -> Self {
        Self {
            dead: Arc::new(AtomicBool::new(false)),
            reason: Arc::new(Mutex::new(None)),
            notify: Arc::new(Notify::new()),
        }
    }

    pub(super) fn mark(
        &self,
        cancel: &CancellationToken,
        conn: &Arc<InteractiveConnection>,
        reason: impl Into<String>,
    ) {
        if !self.dead.swap(true, Ordering::SeqCst) {
            *lock(&self.reason) = Some(reason.into());
            conn.close();
            cancel.cancel();
            self.notify.notify_waiters();
        }
    }

    pub(super) fn is_dead(&self) -> bool {
        self.dead.load(Ordering::SeqCst)
    }

    pub(super) fn reason(&self) -> Option<String> {
        lock(&self.reason).clone()
    }

    pub(super) async fn wait(&self) {
        // Register the waiter (via `enable`) BEFORE checking `dead`, then await. `mark`
        // sets `dead` then calls `notify_waiters()`, which stores no permit — so a `mark`
        // racing between the check and registration would otherwise be lost, leaving the
        // session stuck "connected" to a dead peer with no reconnect. `enable` closes that
        // gap: a `notify_waiters` after `enable` is delivered to this future regardless of
        // its ordering with the `is_dead` check.
        let notified = self.notify.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        if self.is_dead() {
            return;
        }
        notified.await;
    }
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}
