//! App-level **keep-newest** pointer-motion sender (§8 "coalesce keep-newest", §7.6).
//!
//! quinn's datagram send buffer is drop-*oldest*: under load the newest cursor
//! position queues behind a backlog of stale ones, causing rubber-banding. This
//! module enforces the spec's keep-*newest* policy at the application layer with a
//! single-slot mailbox: each new position **overwrites** any still-pending one, and a
//! background pump forwards only the latest position whenever the connection has
//! datagram capacity (`send_datagram_wait`). Combined with a small
//! `datagram_send_buffer_size` (set in [`crate::transport`]), at most a couple of
//! frames are ever in flight and the receiver always converges on the newest sample.

use bytes::Bytes;
use quinn::Connection;
use tokio::sync::watch;
use tokio::task::JoinHandle;

/// A single-slot keep-newest motion sender bound to one [`Connection`].
///
/// [`MotionSender::send`] is non-blocking and infallible: it replaces the pending
/// datagram with the newest one. A background pump task drains the slot to the wire as
/// capacity allows; it ends when the [`MotionSender`] is dropped or the connection dies.
pub struct MotionSender {
    slot: watch::Sender<Option<Bytes>>,
    pump: JoinHandle<()>,
}

impl MotionSender {
    /// Spawn the keep-newest pump for `connection`.
    pub fn spawn(connection: Connection) -> Self {
        let (slot, rx) = watch::channel(None);
        let pump = tokio::spawn(pump(connection, rx));
        Self { slot, pump }
    }

    /// Replace the pending motion datagram with `bytes` (keep-newest). Never blocks;
    /// a stale pending sample is silently overwritten.
    pub fn send(&self, bytes: Bytes) {
        // `send_replace` overwrites the slot and wakes the pump even with no receiver
        // logic depending on the previous value.
        let _ = self.slot.send_replace(Some(bytes));
    }
}

impl Drop for MotionSender {
    fn drop(&mut self) {
        // Dropping the watch sender makes `changed()` in the pump error out, ending it.
        self.pump.abort();
    }
}

/// The pump body: forward the newest pending datagram whenever capacity allows.
async fn pump(connection: Connection, mut rx: watch::Receiver<Option<Bytes>>) {
    loop {
        // Wait for a new (or first) position. Errors only when the sender is dropped.
        if rx.changed().await.is_err() {
            return;
        }
        // Always read the *latest* value, coalescing any samples that arrived while we
        // were blocked — this is the keep-newest guarantee.
        let pending = rx.borrow_and_update().clone();
        let Some(bytes) = pending else {
            continue;
        };
        // `send_datagram_wait` applies backpressure until buffer space exists, so we
        // never pile up behind a backlog. A send error means the connection is gone.
        if connection.send_datagram_wait(bytes).await.is_err() {
            return;
        }
    }
}
