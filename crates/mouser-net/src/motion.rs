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
//!
//! **Error-kind fallback (C2-7, §6.1).** A datagram send can fail four ways
//! ([`quinn::SendDatagramError`]); they are *not* all fatal:
//! - `ConnectionLost` — the connection is genuinely dead → end the pump.
//! - `TooLarge` — this one sample exceeds the current path MTU → drop *it* and keep
//!   pumping (the next, smaller sample may well fit).
//! - `UnsupportedByPeer` / `Disabled` — the datagram plane is unavailable for the whole
//!   connection → publish [`MotionPlane::ControlFallback`] on the [`MotionSender::plane`]
//!   watch so the engine can degrade pointer motion onto the coalesced control stream
//!   (§6.1), and end the pump (there is nothing more it can usefully do).

use bytes::Bytes;
use quinn::{Connection, SendDatagramError};
use tokio::sync::watch;
use tokio::task::JoinHandle;

/// Which transport the pointer-motion plane is currently using (C2-7 / §6.1).
///
/// The pump publishes this on a [`watch`] channel; the engine reads it to decide whether
/// to keep emitting datagrams or to degrade motion onto the control stream.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MotionPlane {
    /// The QUIC DATAGRAM plane is in use (RFC 9221) — the default, low-latency path.
    Datagram,
    /// The datagram plane is unavailable for this connection (`UnsupportedByPeer` /
    /// `Disabled`); the caller MUST send motion over the control stream with coalescing.
    ControlFallback,
}

/// A single-slot keep-newest motion sender bound to one [`Connection`].
///
/// [`MotionSender::send`] is non-blocking and infallible: it replaces the pending
/// datagram with the newest one. A background pump task drains the slot to the wire as
/// capacity allows; it ends when the [`MotionSender`] is dropped, the connection dies, or
/// the peer doesn't support datagrams (in which case [`MotionSender::plane`] flips to
/// [`MotionPlane::ControlFallback`]).
pub struct MotionSender {
    slot: watch::Sender<Option<Bytes>>,
    plane: watch::Receiver<MotionPlane>,
    pump: JoinHandle<()>,
}

impl MotionSender {
    /// Spawn the keep-newest pump for `connection`.
    pub fn spawn(connection: Connection) -> Self {
        let (slot, rx) = watch::channel(None);
        let (plane_tx, plane) = watch::channel(MotionPlane::Datagram);
        let pump = tokio::spawn(pump(connection, rx, plane_tx));
        Self { slot, plane, pump }
    }

    /// Replace the pending motion datagram with `bytes` (keep-newest). Never blocks;
    /// a stale pending sample is silently overwritten.
    pub fn send(&self, bytes: Bytes) {
        // `send_replace` overwrites the slot and wakes the pump even with no receiver
        // logic depending on the previous value.
        let _ = self.slot.send_replace(Some(bytes));
    }

    /// The current motion transport (C2-7). When this reads
    /// [`MotionPlane::ControlFallback`] the datagram plane is unavailable and the caller
    /// must route `PointerMotion` over the control stream (§6.1). Cheap to clone/poll;
    /// the engine can also `changed().await` it to react to a mid-session degrade.
    pub fn plane(&self) -> watch::Receiver<MotionPlane> {
        self.plane.clone()
    }
}

impl Drop for MotionSender {
    fn drop(&mut self) {
        // Dropping the watch sender makes `changed()` in the pump error out, ending it.
        self.pump.abort();
    }
}

/// The pump body: forward the newest pending datagram whenever capacity allows, applying
/// the C2-7 error-kind policy. `plane` carries the degrade signal back to the caller.
async fn pump(
    connection: Connection,
    mut rx: watch::Receiver<Option<Bytes>>,
    plane: watch::Sender<MotionPlane>,
) {
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
        // never pile up behind a backlog. Classify any error (C2-7):
        match connection.send_datagram_wait(bytes).await {
            Ok(()) => {}
            // The path can't carry this particular datagram right now; drop just this
            // sample and keep pumping — a later, smaller sample may fit.
            Err(SendDatagramError::TooLarge) => continue,
            // The datagram plane is unavailable for the whole connection. Signal the
            // caller to degrade onto the control stream (§6.1), then stop pumping.
            Err(SendDatagramError::UnsupportedByPeer) | Err(SendDatagramError::Disabled) => {
                let _ = plane.send(MotionPlane::ControlFallback);
                return;
            }
            // The connection is gone — nothing more to do.
            Err(SendDatagramError::ConnectionLost(_)) => return,
        }
    }
}
