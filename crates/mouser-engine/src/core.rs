//! The sans-IO engine core (architecture §4.6, spec §7.4–§7.6).
//!
//! [`EngineCore`] is a **pure state machine**: it holds no clock, opens no sockets,
//! and touches no OS. It consumes events (local input, decoded wire messages, motion
//! datagrams, and time ticks) and returns a list of [`Action`]s for the async runtime
//! to execute (send a frame, send a motion datagram, inject into the local OS, or set
//! the capture suppress/passthrough decision). This mirrors the rest of the codebase's
//! pure-core style (`mouser-core::Ownership`, `mouser-files`) so the hard logic —
//! edge-crossing, ownership handoff, anti-replay, heartbeat-timeout reclaim — is fully
//! unit-testable without IO.
//!
//! ## Roles (v1, single peer)
//! - [`Role::Source`] is the machine with the physical keyboard/mouse. It captures
//!   local input; while the cursor is on its own screen it passes input through, and
//!   when the cursor crosses the configured edge it grants ownership to the peer and
//!   forwards input (suppressing it locally).
//! - [`Role::Target`] injects input it receives while it is the owner; it does not
//!   capture. (Both roles run [`EngineCore::on_control`]/[`on_motion`]/[`on_tick`].)

use mouser_core::platform::{CaptureMode, LocalInputEvent, ScrollUnit as CoreScrollUnit};
use mouser_core::{
    ownership::{Ownership, OwnershipUpdate},
    DeviceId,
};
use mouser_protocol::{
    from_cbor, to_cbor, Datagram, FocusKind, Goodbye, GoodbyeReason, Heartbeat, KeyEvent,
    OwnershipAck, OwnershipTransfer, Ping, PointerButton, PointerMotion, Pong, Scroll, ScrollUnit,
    TransferReason, TYPE_GOODBYE, TYPE_HEARTBEAT, TYPE_KEY_EVENT, TYPE_OWNERSHIP_ACK,
    TYPE_OWNERSHIP_TRANSFER, TYPE_PING, TYPE_POINTER_BUTTON, TYPE_PONG, TYPE_SCROLL,
};

/// Heartbeat misses before the source declares the peer gone and reclaims (spec §7;
/// 1 s tick × 3 = 3 s timeout).
const HEARTBEAT_MISS_LIMIT: u32 = 3;

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
        Self {
            width,
            height,
            peer_width,
            peer_height,
            edge: Edge::Right,
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
    /// Transition the capture adapter's mode (the "edge sensing is not input
    /// forwarding" lever). Emitted whenever ownership changes so the runtime
    /// installs heavyweight forwarding hooks **only** while actively controlling a
    /// remote peer and otherwise sits in passive, non-suppressing edge sensing.
    SetCaptureMode(CaptureMode),
    /// Ownership/owner changed — for the tray/UI and logging.
    OwnerChanged { owner: DeviceId, epoch: u64 },
}

/// Per-direction anti-replay state for the current epoch (spec §7.5/§7.6).
#[derive(Clone, Copy, Debug, Default)]
struct ReplayGuard {
    epoch: u64,
    last_ctr: Option<u64>,
    last_seq: Option<u32>,
}

impl ReplayGuard {
    /// Accept a control event iff its epoch is current and `ctr` strictly increases.
    fn accept_ctr(&mut self, epoch: u64, ctr: u64, current_epoch: u64) -> bool {
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
    fn accept_seq(&mut self, epoch: u64, seq: u32, current_epoch: u64) -> bool {
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

/// The pure engine state machine.
pub struct EngineCore {
    role: Role,
    peer: DeviceId,
    ownership: Ownership,
    layout: EdgeLayout,
    /// Outgoing per-epoch counters (reset whenever we mint/adopt a new epoch).
    out_ctr: u64,
    out_seq: u32,
    /// Incoming anti-replay guard (peer → us).
    guard: ReplayGuard,
    /// Virtual peer cursor while we own the peer (absolute, peer display space).
    peer_x: i32,
    peer_y: i32,
    /// Last local cursor position seen, for delta computation while forwarding.
    last_local: Option<(i32, i32)>,
    /// Ticks since the last heartbeat from the peer.
    misses: u32,
    /// Our outgoing heartbeat sequence.
    hb_seq: u64,
}

impl EngineCore {
    /// A source node: `me` has the physical input; `peer` is the target across `layout.edge`.
    pub fn new_source(me: DeviceId, peer: DeviceId, layout: EdgeLayout) -> Self {
        Self::new(Role::Source, me, peer, layout)
    }

    /// A target node: injects input received from `peer` while it owns input.
    pub fn new_target(me: DeviceId, peer: DeviceId) -> Self {
        Self::new(Role::Target, me, peer, EdgeLayout::side_by_side(0, 0, 0, 0))
    }

    fn new(role: Role, me: DeviceId, peer: DeviceId, layout: EdgeLayout) -> Self {
        Self {
            role,
            peer,
            ownership: Ownership::new(me),
            layout,
            out_ctr: 0,
            out_seq: 0,
            guard: ReplayGuard::default(),
            peer_x: 0,
            peer_y: 0,
            last_local: None,
            misses: 0,
            hb_seq: 0,
        }
    }

    /// The currently-known owner.
    pub fn owner(&self) -> DeviceId {
        self.ownership.owner()
    }

    /// The current ownership epoch.
    pub fn epoch(&self) -> u64 {
        self.ownership.epoch()
    }

    /// Whether this node currently owns input.
    pub fn is_owner(&self) -> bool {
        self.ownership.is_owner()
    }

    /// The capture mode this node should be in **right now**, derived purely from
    /// role + ownership (a total function of state — never a stored flag, so it can
    /// never drift out of sync with ownership):
    ///
    /// - [`Role::Target`] → always [`CaptureMode::Off`] (it injects, never captures).
    /// - [`Role::Source`] while it owns input (cursor on its own screen) →
    ///   [`CaptureMode::PassiveEdge`]: sense the edge with no hooks, no suppression,
    ///   so local keyboard/touchpad are untouched.
    /// - [`Role::Source`] while a peer owns input (we are forwarding) →
    ///   [`CaptureMode::ActiveForward`]: full suppressing capture.
    #[must_use]
    pub fn capture_mode(&self) -> CaptureMode {
        match self.role {
            Role::Target => CaptureMode::Off,
            Role::Source if self.is_owner() => CaptureMode::PassiveEdge,
            Role::Source => CaptureMode::ActiveForward,
        }
    }

    /// The actions the runtime must apply once when the session starts, to bring
    /// capture up in the correct initial mode (a source begins in
    /// [`CaptureMode::PassiveEdge`]; a target in [`CaptureMode::Off`]). Keeping this
    /// in the core means the "what mode now?" decision lives in exactly one place.
    #[must_use]
    pub fn initial_actions(&self) -> Vec<Action> {
        vec![Action::SetCaptureMode(self.capture_mode())]
    }

    fn me(&self) -> DeviceId {
        self.ownership.me()
    }

    /// Reset outgoing counters on every epoch change (anti-replay, §7.5/§7.6).
    fn reset_out(&mut self) {
        self.out_ctr = 0;
        self.out_seq = 0;
    }

    fn next_ctr(&mut self) -> u64 {
        self.out_ctr = self.out_ctr.saturating_add(1);
        self.out_ctr
    }

    fn next_seq(&mut self) -> u32 {
        self.out_seq = self.out_seq.saturating_add(1);
        self.out_seq
    }

    /// Process a captured local input event (source only). Decides edge-crossing,
    /// forwarding, and local suppress/passthrough.
    pub fn on_local_input(&mut self, event: LocalInputEvent) -> Vec<Action> {
        if self.role != Role::Source {
            return vec![Action::Capture(CaptureDecision::PassThrough)];
        }
        let owns = self.is_owner();
        match event {
            LocalInputEvent::CursorMoved { x, y, .. } => self.on_cursor(x, y, owns),
            other if owns => {
                // Cursor is on our screen — let our own input drive our desktop.
                let _ = other;
                vec![Action::Capture(CaptureDecision::PassThrough)]
            }
            LocalInputEvent::Key { usage, down, mods } => {
                let ctr = self.next_ctr();
                let payload = encode(&KeyEvent {
                    usage,
                    down,
                    mods,
                    owner_epoch: self.epoch(),
                    ctr,
                });
                vec![
                    Action::SendControl(TYPE_KEY_EVENT, payload),
                    Action::Capture(CaptureDecision::Suppress),
                ]
            }
            LocalInputEvent::Button { button, down } => {
                let ctr = self.next_ctr();
                let payload = encode(&PointerButton {
                    button,
                    down,
                    owner_epoch: self.epoch(),
                    ctr,
                });
                vec![
                    Action::SendControl(TYPE_POINTER_BUTTON, payload),
                    Action::Capture(CaptureDecision::Suppress),
                ]
            }
            LocalInputEvent::Scroll { dx, dy } => {
                let ctr = self.next_ctr();
                let payload = encode(&Scroll {
                    dx,
                    dy,
                    unit: ScrollUnit::LogicalPixel,
                    owner_epoch: self.epoch(),
                    ctr,
                });
                vec![
                    Action::SendControl(TYPE_SCROLL, payload),
                    Action::Capture(CaptureDecision::Suppress),
                ]
            }
        }
    }

    fn on_cursor(&mut self, x: i32, y: i32, owns: bool) -> Vec<Action> {
        let prev = self.last_local.replace((x, y));
        if owns {
            // On our own screen: cross to the peer when we reach the configured edge.
            if self.crosses_out(x, y) {
                return self.cross_to_peer(y);
            }
            return vec![Action::Capture(CaptureDecision::PassThrough)];
        }
        // We own the peer: translate motion into the peer's space and forward it.
        let (dx, dy) = match prev {
            Some((px, py)) => (x - px, y - py),
            None => (0, 0),
        };
        self.peer_x = clamp(
            self.peer_x + dx,
            0,
            self.layout.peer_width.saturating_sub(1).max(0),
        );
        self.peer_y = clamp(
            self.peer_y + dy,
            0,
            self.layout.peer_height.saturating_sub(1).max(0),
        );
        // Crossing back: the peer cursor hit the near edge moving toward us.
        if self.crosses_back(dx) {
            return self.reclaim_local();
        }
        let seq = self.next_seq();
        let motion = PointerMotion {
            owner_epoch: self.epoch(),
            seq,
            display_id: 0,
            x: self.peer_x,
            y: self.peer_y,
        };
        vec![
            Action::SendMotion(motion),
            Action::Capture(CaptureDecision::Suppress),
        ]
    }

    /// Does `(x, y)` reach the edge the peer sits on?
    fn crosses_out(&self, x: i32, y: i32) -> bool {
        match self.layout.edge {
            Edge::Right => x >= self.layout.width.saturating_sub(1),
            Edge::Left => x <= 0,
            Edge::Bottom => y >= self.layout.height.saturating_sub(1),
            Edge::Top => y <= 0,
        }
    }

    /// Has the peer cursor returned to the near edge (back toward us)?
    fn crosses_back(&self, delta_along: i32) -> bool {
        match self.layout.edge {
            Edge::Right => self.peer_x <= 0 && delta_along < 0,
            Edge::Left => {
                self.peer_x >= self.layout.peer_width.saturating_sub(1) && delta_along > 0
            }
            // Vertical edges use the y delta; kept simple for v1 (x delta proxy unused).
            Edge::Bottom => self.peer_y <= 0,
            Edge::Top => self.peer_y >= self.layout.peer_height.saturating_sub(1),
        }
    }

    fn cross_to_peer(&mut self, y: i32) -> Vec<Action> {
        let Some(epoch) = self.ownership.grant_to(self.peer) else {
            return vec![Action::Capture(CaptureDecision::PassThrough)];
        };
        self.reset_out();
        self.guard = ReplayGuard::default();
        // Seed the peer cursor at the entry edge.
        self.peer_x = match self.layout.edge {
            Edge::Right => 0,
            Edge::Left => self.layout.peer_width.saturating_sub(1).max(0),
            _ => clamp(
                self.peer_x,
                0,
                self.layout.peer_width.saturating_sub(1).max(0),
            ),
        };
        self.peer_y = clamp(y, 0, self.layout.peer_height.saturating_sub(1).max(0));
        let transfer = encode(&OwnershipTransfer {
            to: self.peer.to_vec(),
            owner_epoch: epoch,
            layout_rev: 0,
            reason: TransferReason::EdgeCross,
        });
        let seq = self.next_seq();
        let motion = PointerMotion {
            owner_epoch: epoch,
            seq,
            display_id: 0,
            x: self.peer_x,
            y: self.peer_y,
        };
        // SetCaptureMode goes first: the runtime escalates to ActiveForward (installs
        // suppressing hooks) before we start suppressing/forwarding this crossing.
        vec![
            Action::SetCaptureMode(CaptureMode::ActiveForward),
            Action::SendControl(TYPE_OWNERSHIP_TRANSFER, transfer),
            Action::SendMotion(motion),
            Action::OwnerChanged {
                owner: self.peer,
                epoch,
            },
            Action::Capture(CaptureDecision::Suppress),
        ]
    }

    fn reclaim_local(&mut self) -> Vec<Action> {
        let epoch = self.ownership.reclaim();
        self.reset_out();
        self.guard = ReplayGuard::default();
        let me = self.me();
        let transfer = encode(&OwnershipTransfer {
            to: me.to_vec(),
            owner_epoch: epoch,
            layout_rev: 0,
            reason: TransferReason::LocalReclaim,
        });
        // SetCaptureMode goes first: drop suppressing hooks back to passive edge
        // sensing the moment we reclaim, so local input is immediately untouched.
        vec![
            Action::SetCaptureMode(CaptureMode::PassiveEdge),
            Action::SendControl(TYPE_OWNERSHIP_TRANSFER, transfer),
            Action::OwnerChanged { owner: me, epoch },
            Action::Capture(CaptureDecision::PassThrough),
        ]
    }

    /// Process a decoded control frame from the peer.
    pub fn on_control(&mut self, msg_type: u16, payload: &[u8]) -> Vec<Action> {
        match msg_type {
            TYPE_OWNERSHIP_TRANSFER => self.on_transfer(payload),
            TYPE_OWNERSHIP_ACK => Vec::new(), // ack tracking is best-effort in v1
            TYPE_KEY_EVENT => self.on_key(payload),
            TYPE_POINTER_BUTTON => self.on_button(payload),
            TYPE_SCROLL => self.on_scroll(payload),
            TYPE_PING => self.on_ping(payload),
            TYPE_PONG | TYPE_HEARTBEAT => {
                self.misses = 0;
                Vec::new()
            }
            TYPE_GOODBYE => self.on_goodbye(payload),
            _ => Vec::new(), // unknown type: skippable (§2 forward-compat)
        }
    }

    fn on_transfer(&mut self, payload: &[u8]) -> Vec<Action> {
        let Ok(t) = from_cbor::<OwnershipTransfer>(payload) else {
            return Vec::new();
        };
        let Some(owner) = to_id(&t.to) else {
            return Vec::new();
        };
        match self
            .ownership
            .observe_with_reason(owner, t.owner_epoch, t.reason)
        {
            OwnershipUpdate::Accepted { owner, epoch } => {
                self.guard = ReplayGuard::default();
                self.reset_out();
                self.misses = 0;
                let ack = encode(&OwnershipAck {
                    owner_epoch: epoch,
                    accepted: true,
                    reason: None,
                });
                let mut actions = vec![
                    Action::SendControl(TYPE_OWNERSHIP_ACK, ack),
                    Action::OwnerChanged { owner, epoch },
                ];
                // An inbound transfer can flip a Source between owning (passive edge
                // sensing) and forwarding (active capture) — e.g. a peer reclaiming
                // on its side. Re-sync the capture mode to the new ownership so a
                // Source that just lost the input stops its suppressing hooks (and
                // one that regained it drops back to passive). Idempotent in the
                // adapter, so a no-op when the mode is unchanged. A Target never
                // captures, so it emits nothing here.
                if self.role == Role::Source {
                    actions.push(Action::SetCaptureMode(self.capture_mode()));
                }
                actions
            }
            OwnershipUpdate::Rejected(_) => {
                let ack = encode(&OwnershipAck {
                    owner_epoch: t.owner_epoch,
                    accepted: false,
                    reason: Some("stale epoch".to_string()),
                });
                vec![Action::SendControl(TYPE_OWNERSHIP_ACK, ack)]
            }
        }
    }

    fn on_key(&mut self, payload: &[u8]) -> Vec<Action> {
        let Ok(k) = from_cbor::<KeyEvent>(payload) else {
            return Vec::new();
        };
        if !self.is_owner() || !self.guard.accept_ctr(k.owner_epoch, k.ctr, self.epoch()) {
            return Vec::new();
        }
        vec![Action::Inject(Inject::Key {
            usage: k.usage,
            down: k.down,
            mods: k.mods,
        })]
    }

    fn on_button(&mut self, payload: &[u8]) -> Vec<Action> {
        let Ok(b) = from_cbor::<PointerButton>(payload) else {
            return Vec::new();
        };
        if !self.is_owner() || !self.guard.accept_ctr(b.owner_epoch, b.ctr, self.epoch()) {
            return Vec::new();
        }
        vec![Action::Inject(Inject::Button {
            button: b.button,
            down: b.down,
        })]
    }

    fn on_scroll(&mut self, payload: &[u8]) -> Vec<Action> {
        let Ok(s) = from_cbor::<Scroll>(payload) else {
            return Vec::new();
        };
        if !self.is_owner() || !self.guard.accept_ctr(s.owner_epoch, s.ctr, self.epoch()) {
            return Vec::new();
        }
        let unit = match s.unit {
            ScrollUnit::Detent120 => CoreScrollUnit::Detent120,
            _ => CoreScrollUnit::LogicalPixel,
        };
        vec![Action::Inject(Inject::Scroll {
            dx: s.dx,
            dy: s.dy,
            unit,
        })]
    }

    fn on_ping(&mut self, payload: &[u8]) -> Vec<Action> {
        self.misses = 0;
        let Ok(p) = from_cbor::<Ping>(payload) else {
            return Vec::new();
        };
        vec![Action::SendControl(TYPE_PONG, encode(&Pong { id: p.id }))]
    }

    fn on_goodbye(&mut self, payload: &[u8]) -> Vec<Action> {
        let _ = from_cbor::<Goodbye>(payload);
        // Treat a peer Goodbye like a disconnect: the source reclaims its input.
        if self.role == Role::Source && !self.is_owner() {
            return self.reclaim_local();
        }
        Vec::new()
    }

    /// Process a pointer-motion datagram from the peer.
    pub fn on_motion(&mut self, datagram: Datagram) -> Vec<Action> {
        let Datagram::Motion(m) = datagram else {
            return Vec::new();
        };
        if !self.is_owner() || !self.guard.accept_seq(m.owner_epoch, m.seq, self.epoch()) {
            return Vec::new();
        }
        vec![Action::Inject(Inject::MoveCursor {
            display_id: m.display_id,
            x: m.x,
            y: m.y,
        })]
    }

    /// Advance time one heartbeat interval (~1 s). Emits our heartbeat and, on the
    /// source, reclaims input if the peer has gone silent past the miss limit.
    pub fn on_tick(&mut self) -> Vec<Action> {
        let mut actions = Vec::new();
        self.hb_seq = self.hb_seq.saturating_add(1);
        actions.push(Action::SendControl(
            TYPE_HEARTBEAT,
            encode(&Heartbeat { seq: self.hb_seq }),
        ));
        self.misses = self.misses.saturating_add(1);
        if self.role == Role::Source && !self.is_owner() && self.misses >= HEARTBEAT_MISS_LIMIT {
            actions.extend(self.reclaim_local());
        }
        actions
    }

    /// Build a Goodbye frame (e.g. on sleep/quit) for the runtime to send.
    pub fn goodbye(reason: GoodbyeReason) -> (u16, Vec<u8>) {
        (TYPE_GOODBYE, encode(&Goodbye { reason }))
    }

    /// Current focus state (for the tray).
    pub fn focus(&self) -> FocusKind {
        self.ownership.focus()
    }
}

/// Encode a control payload; on the (practically impossible) CBOR failure of a
/// fixed-shape struct, fall back to empty bytes rather than panic (panic-free core).
fn encode<T: serde::Serialize>(value: &T) -> Vec<u8> {
    to_cbor(value).unwrap_or_default()
}

/// Parse a wire `bytes32` device id; `None` if it is not exactly 32 bytes.
fn to_id(bytes: &[u8]) -> Option<DeviceId> {
    <[u8; 32]>::try_from(bytes).ok()
}

fn clamp(v: i32, lo: i32, hi: i32) -> i32 {
    v.max(lo).min(hi.max(lo))
}
