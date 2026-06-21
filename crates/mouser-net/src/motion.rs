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

/// What the pump should do after a [`Connection::send_datagram_wait`] result (C2-7).
///
/// Extracted from the pump loop so the [`SendDatagramError`] policy can be unit-tested
/// exhaustively without a live [`Connection`]: see [`classify`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PumpAction {
    /// Keep pumping — either the send succeeded or only this one sample was rejected
    /// (`TooLarge`); a later, smaller sample may fit.
    Continue,
    /// The datagram plane is unavailable for the whole connection
    /// (`UnsupportedByPeer` / `Disabled`): publish [`MotionPlane::ControlFallback`] and
    /// stop pumping (§6.1).
    Fallback,
    /// The connection is gone (`ConnectionLost`): stop pumping, nothing more to do.
    Exit,
}

/// Map a datagram-send error to the pump's reaction (C2-7, §6.1). Pure and total over
/// every [`SendDatagramError`] variant so the policy is unit-testable in isolation.
fn classify(err: &SendDatagramError) -> PumpAction {
    match err {
        // The path can't carry this particular datagram right now; drop just this sample
        // and keep pumping — a later, smaller sample may fit.
        SendDatagramError::TooLarge => PumpAction::Continue,
        // The datagram plane is unavailable for the whole connection. Signal the caller to
        // degrade onto the control stream (§6.1), then stop pumping.
        SendDatagramError::UnsupportedByPeer | SendDatagramError::Disabled => PumpAction::Fallback,
        // The connection is gone — nothing more to do.
        SendDatagramError::ConnectionLost(_) => PumpAction::Exit,
    }
}

/// Carry out a [`PumpAction`] against the motion-plane watch, returning whether the pump
/// should keep looping.
///
/// This is the *only* place the [`MotionPlane::ControlFallback`] degrade signal is
/// published, so unit-testing it (with no live [`Connection`]) fully covers the C2-7
/// side effect: `Fallback` publishes the signal and stops; `Exit` stops silently;
/// `Continue` keeps the pump running without touching the plane.
fn react(plane: &watch::Sender<MotionPlane>, action: PumpAction) -> bool {
    match action {
        PumpAction::Continue => true,
        // Signal the caller to degrade onto the control stream (§6.1), then stop.
        PumpAction::Fallback => {
            let _ = plane.send(MotionPlane::ControlFallback);
            false
        }
        PumpAction::Exit => false,
    }
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
        // `send_datagram_wait` applies backpressure until buffer space exists, so we never
        // pile up behind a backlog. On error, the C2-7 policy lives in `classify` and its
        // side effect in `react`:
        if let Err(err) = connection.send_datagram_wait(bytes).await {
            if !react(&plane, classify(&err)) {
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{classify, react, MotionPlane, PumpAction};
    use quinn::{ConnectionError, SendDatagramError};
    use tokio::sync::watch;

    /// `ConnectionLost` wraps a [`ConnectionError`]; `Reset` is a convenient unit variant
    /// to build a representative instance for the test.
    fn connection_lost() -> SendDatagramError {
        SendDatagramError::ConnectionLost(ConnectionError::Reset)
    }

    /// C2-7: every `SendDatagramError` variant maps to the spec'd pump reaction. This is
    /// exhaustive over the real quinn enum — a regressed branch (e.g. `TooLarge` becoming
    /// fatal, or `UnsupportedByPeer` no longer triggering fallback) fails here.
    #[test]
    fn classify_covers_every_send_datagram_error_variant() {
        // `TooLarge` → drop one sample, keep pumping.
        assert_eq!(
            classify(&SendDatagramError::TooLarge),
            PumpAction::Continue,
            "TooLarge must drop the sample and continue, not abort the pump"
        );
        // Plane-wide unavailability → degrade onto the control stream.
        assert_eq!(
            classify(&SendDatagramError::UnsupportedByPeer),
            PumpAction::Fallback,
            "UnsupportedByPeer must trigger ControlFallback"
        );
        assert_eq!(
            classify(&SendDatagramError::Disabled),
            PumpAction::Fallback,
            "Disabled must trigger ControlFallback"
        );
        // Dead connection → stop, no degrade.
        assert_eq!(
            classify(&connection_lost()),
            PumpAction::Exit,
            "ConnectionLost must end the pump without a fallback"
        );

        // Exhaustiveness guard: if quinn adds a variant, this match stops compiling and
        // forces the policy (and the asserts above) to be revisited.
        let _exhaustive = |e: &SendDatagramError| match e {
            SendDatagramError::TooLarge => (),
            SendDatagramError::UnsupportedByPeer => (),
            SendDatagramError::Disabled => (),
            SendDatagramError::ConnectionLost(_) => (),
        };
    }

    /// `react(Fallback)` is the *only* producer of [`MotionPlane::ControlFallback`]; it must
    /// publish it on the watch and tell the pump to stop. A receiver observes the degrade,
    /// exactly as the engine would (§6.1).
    #[test]
    fn react_fallback_publishes_control_fallback_and_stops() {
        let (tx, mut rx) = watch::channel(MotionPlane::Datagram);
        assert_eq!(*rx.borrow_and_update(), MotionPlane::Datagram);

        let keep_going = react(&tx, PumpAction::Fallback);

        assert!(!keep_going, "Fallback must end the pump");
        assert!(rx.has_changed().unwrap_or(false), "plane must have changed");
        assert_eq!(
            *rx.borrow(),
            MotionPlane::ControlFallback,
            "Fallback must publish MotionPlane::ControlFallback for the engine to degrade onto control"
        );
    }

    /// `react(Continue)` keeps the pump running and never touches the plane (it stays
    /// `Datagram`) — a `TooLarge` drop must not be mistaken for a degrade.
    #[test]
    fn react_continue_keeps_pumping_without_degrading() {
        let (tx, rx) = watch::channel(MotionPlane::Datagram);

        let keep_going = react(&tx, PumpAction::Continue);

        assert!(keep_going, "Continue must keep the pump running");
        assert!(
            !rx.has_changed().unwrap_or(true),
            "Continue must not publish on the plane"
        );
        assert_eq!(
            *rx.borrow(),
            MotionPlane::Datagram,
            "plane stays on Datagram"
        );
    }

    /// `react(Exit)` stops the pump but, unlike `Fallback`, publishes nothing: a dead
    /// connection is not a transport degrade.
    #[test]
    fn react_exit_stops_without_publishing() {
        let (tx, rx) = watch::channel(MotionPlane::Datagram);

        let keep_going = react(&tx, PumpAction::Exit);

        assert!(!keep_going, "Exit must end the pump");
        assert!(
            !rx.has_changed().unwrap_or(true),
            "Exit must not publish on the plane"
        );
        assert_eq!(
            *rx.borrow(),
            MotionPlane::Datagram,
            "plane stays on Datagram"
        );
    }

    /// End-to-end over the *wired* path the pump uses (`classify` → `react`): each real
    /// `SendDatagramError` drives the plane to the spec'd terminal state. Only the two
    /// plane-wide errors degrade to `ControlFallback`; `TooLarge` and `ConnectionLost`
    /// leave the plane on `Datagram`.
    #[test]
    fn pump_policy_drives_plane_per_error_kind() {
        let cases = [
            (SendDatagramError::TooLarge, MotionPlane::Datagram, true),
            (
                SendDatagramError::UnsupportedByPeer,
                MotionPlane::ControlFallback,
                false,
            ),
            (
                SendDatagramError::Disabled,
                MotionPlane::ControlFallback,
                false,
            ),
            (connection_lost(), MotionPlane::Datagram, false),
        ];

        for (err, expected_plane, expected_keep_going) in cases {
            let (tx, rx) = watch::channel(MotionPlane::Datagram);
            let keep_going = react(&tx, classify(&err));
            assert_eq!(
                keep_going, expected_keep_going,
                "wrong keep-going decision for {err:?}"
            );
            assert_eq!(
                *rx.borrow(),
                expected_plane,
                "wrong plane state after handling {err:?}"
            );
        }
    }
}
