use mouser_core::platform::{CaptureMode, ScrollUnit as CoreScrollUnit};
use mouser_core::DeviceId;
use mouser_protocol::PointerMotion;

/// Heartbeat ticks to wait for a positive ownership ACK before reclaiming.
pub(super) const OWNERSHIP_ACK_TICKS: u8 = 2;

/// Maximum input events accepted from the peer per heartbeat window.
const INPUT_RATE_BURST: u16 = 240;

/// Tokens replenished on each heartbeat tick.
const INPUT_RATE_REFILL: u16 = 120;

/// Which side of this machine the peer's screen sits on (the crossing edge).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Edge {
    Left,
    Right,
    Top,
    Bottom,
}

/// This node's role in the v1 single-peer topology.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    /// Has the physical input; captures and forwards across the edge.
    Source,
    /// Injects received input while it is the owner; does not capture.
    Target,
}

/// Source-side screen geometry: this machine's logical-pixel size, the peer's size,
/// and the edge the peer sits on. Used for edge-crossing and to seed the peer cursor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EdgeLayout {
    pub width: i32,
    pub height: i32,
    pub peer_width: i32,
    pub peer_height: i32,
    pub edge: Edge,
}

impl EdgeLayout {
    /// A symmetric side-by-side layout with the peer on the right.
    pub fn side_by_side(width: i32, height: i32, peer_width: i32, peer_height: i32) -> Self {
        Self::with_edge(width, height, peer_width, peer_height, Edge::Right)
    }

    /// A layout with the peer on a chosen `edge` (the edge the cursor crosses to reach it).
    pub fn with_edge(
        width: i32,
        height: i32,
        peer_width: i32,
        peer_height: i32,
        edge: Edge,
    ) -> Self {
        Self {
            width,
            height,
            peer_width,
            peer_height,
            edge,
        }
    }
}

impl Edge {
    /// Parse a settings string (`"left" | "right" | "top" | "bottom"`) into an [`Edge`],
    /// defaulting to [`Edge::Right`] for anything unrecognized.
    #[must_use]
    pub fn from_setting(s: &str) -> Self {
        match s {
            "left" => Edge::Left,
            "top" => Edge::Top,
            "bottom" => Edge::Bottom,
            _ => Edge::Right,
        }
    }
}

/// A concrete injection the runtime applies via [`mouser_core::platform::InputInjection`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Inject {
    MoveCursor {
        display_id: u32,
        x: i32,
        y: i32,
    },
    Button {
        button: u8,
        down: bool,
    },
    Key {
        usage: u16,
        down: bool,
        mods: u16,
    },
    Scroll {
        dx: i32,
        dy: i32,
        unit: CoreScrollUnit,
    },
}

/// What the capture adapter should do with the local event just processed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CaptureDecision {
    PassThrough,
    Suppress,
}

/// An instruction the runtime must carry out.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    /// Send a framed control message: `(type, CBOR payload)`.
    SendControl(u16, Vec<u8>),
    /// Send a lossy pointer-motion datagram.
    SendMotion(PointerMotion),
    /// Inject synthetic input locally.
    Inject(Inject),
    /// Tell the capture adapter to pass through or swallow the local event.
    Capture(CaptureDecision),
    /// Transition the capture adapter's mode.
    SetCaptureMode(CaptureMode),
    /// Ownership/owner changed — for the tray/UI and logging.
    OwnerChanged { owner: DeviceId, epoch: u64 },
}

/// Per-direction anti-replay state for the current epoch (spec §7.5/§7.6).
#[derive(Clone, Copy, Debug, Default)]
pub(super) struct ReplayGuard {
    epoch: u64,
    last_ctr: Option<u64>,
    last_seq: Option<u32>,
}

impl ReplayGuard {
    /// Accept a control event iff its epoch is current and `ctr` strictly increases.
    pub(super) fn accept_ctr(&mut self, epoch: u64, ctr: u64, current_epoch: u64) -> bool {
        if epoch != current_epoch {
            return false;
        }
        if self.epoch != epoch {
            self.epoch = epoch;
            self.last_ctr = None;
            self.last_seq = None;
        }
        if self.last_ctr.is_some_and(|prev| ctr <= prev) {
            return false;
        }
        self.last_ctr = Some(ctr);
        true
    }

    /// Accept a motion datagram iff its epoch is current and `seq` strictly increases.
    pub(super) fn accept_seq(&mut self, epoch: u64, seq: u32, current_epoch: u64) -> bool {
        if epoch != current_epoch {
            return false;
        }
        if self.epoch != epoch {
            self.epoch = epoch;
            self.last_ctr = None;
            self.last_seq = None;
        }
        if self.last_seq.is_some_and(|prev| seq <= prev) {
            return false;
        }
        self.last_seq = Some(seq);
        true
    }
}

/// Local authorization for peer-originated input.
#[derive(Clone, Copy, Debug)]
pub(super) struct InputAuth {
    peer_trusted: bool,
    input_allowed: bool,
    accepted_epoch: Option<u64>,
}

impl InputAuth {
    pub(super) fn new_trusted() -> Self {
        Self {
            peer_trusted: true,
            input_allowed: true,
            accepted_epoch: None,
        }
    }

    pub(super) fn set_peer_trusted(&mut self, trusted: bool) {
        self.peer_trusted = trusted;
    }

    pub(super) fn set_input_allowed(&mut self, allowed: bool) {
        self.input_allowed = allowed;
    }

    pub(super) fn authorize_epoch(&mut self, epoch: u64) {
        self.accepted_epoch = Some(epoch);
    }

    pub(super) fn revoke_epoch(&mut self) {
        self.accepted_epoch = None;
    }

    pub(super) fn allows_epoch(&self, epoch: u64) -> bool {
        self.peer_trusted && self.input_allowed && self.accepted_epoch == Some(epoch)
    }

    pub(super) fn can_accept_grant(&self) -> bool {
        self.peer_trusted && self.input_allowed
    }
}

/// Simple single-peer token bucket, refilled by the heartbeat tick.
#[derive(Clone, Copy, Debug)]
pub(super) struct InputRate {
    tokens: u16,
}

impl InputRate {
    pub(super) fn full() -> Self {
        Self {
            tokens: INPUT_RATE_BURST,
        }
    }

    pub(super) fn refill(&mut self) {
        self.tokens = self
            .tokens
            .saturating_add(INPUT_RATE_REFILL)
            .min(INPUT_RATE_BURST);
    }

    pub(super) fn allow_one(&mut self) -> bool {
        if self.tokens == 0 {
            return false;
        }
        self.tokens = self.tokens.saturating_sub(1);
        true
    }
}

/// An outbound ownership grant waiting for the peer's positive ACK.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct PendingAck {
    pub(super) epoch: u64,
    pub(super) ticks_left: u8,
}

impl PendingAck {
    pub(super) fn new(epoch: u64) -> Self {
        Self {
            epoch,
            ticks_left: OWNERSHIP_ACK_TICKS,
        }
    }
}
