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

mod types;
mod wire;

use mouser_core::platform::{CaptureMode, LocalInputEvent};
use mouser_core::{ownership::Ownership, DeviceId};
use mouser_protocol::{
    to_cbor, FocusKind, Goodbye, GoodbyeReason, Heartbeat, KeyEvent, OwnershipTransfer,
    PointerButton, PointerMotion, Scroll, ScrollUnit, TransferReason, TYPE_GOODBYE, TYPE_HEARTBEAT,
    TYPE_KEY_EVENT, TYPE_OWNERSHIP_TRANSFER, TYPE_POINTER_BUTTON, TYPE_SCROLL,
};

pub use types::{Action, CaptureDecision, Edge, EdgeLayout, Inject, Role};
use types::{HeldKeys, InputAuth, InputRate, PendingAck, ReplayGuard};

/// Heartbeat misses before the source declares the peer gone and reclaims (spec §7;
/// 1 s tick × 3 = 3 s timeout).
const HEARTBEAT_MISS_LIMIT: u32 = 3;
/// Distance from the source edge before a reclaimed local cursor may cross out again.
/// This prevents a restore/warp at the shared edge from immediately handing ownership
/// back to the peer before the user has re-entered the local screen.
const EDGE_REARM_MARGIN: i32 = 8;

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
    /// Held peer-originated keys in the current accepted input epoch.
    held_keys: HeldKeys,
    /// Peer-originated input is allowed only after a trusted current-epoch grant.
    input_auth: InputAuth,
    /// Per-peer input burst/rate cap.
    input_rate: InputRate,
    /// Outbound grant awaiting a positive OwnershipAck.
    pending_ack: Option<PendingAck>,
    /// Virtual peer cursor while we own the peer (absolute, peer display space).
    peer_x: i32,
    peer_y: i32,
    /// A back-cross can reclaim only after the peer cursor has first moved away from
    /// the edge where we seeded it. This filters handoff artifacts such as local cursor
    /// parking/warping or first-sample bounce at the entry edge.
    reclaim_armed: bool,
    /// After reclaiming local ownership, outbound crossing is disabled until the
    /// local cursor has moved back inside this display.
    cross_out_armed: bool,
    /// Ticks since the last heartbeat from the peer.
    misses: u32,
    /// Our outgoing heartbeat sequence.
    hb_seq: u64,
    /// Set once the platform reported it can't suppress local input: stop crossing to the
    /// peer for the rest of this session (otherwise the cursor would cross-and-reclaim in a
    /// loop while it rests on the edge). Cleared by a fresh session — the user grants the
    /// permission and reconnects.
    suppress_blocked: bool,
    /// Net home-ward device travel (true deltas, in pixels) accumulated while we own the
    /// peer. Geometry- and absolute-position-independent emergency escape: a sustained
    /// push of one local screen-width toward home reclaims even when `crosses_back` can't
    /// fire (advertised `peer_width` too large, so `peer_x` never reaches its edge) and
    /// `local_escape_back` can't fire (the OS froze the suppressed local cursor at the
    /// entry edge). Reset on every cross; clamped at 0 so wandering inside the peer cancels
    /// out and only a deliberate continuous home-ward swipe accumulates to the threshold.
    escape_travel: i32,
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
            held_keys: HeldKeys::default(),
            input_auth: InputAuth::new_trusted(),
            input_rate: InputRate::full(),
            pending_ack: None,
            peer_x: 0,
            peer_y: 0,
            reclaim_armed: true,
            cross_out_armed: true,
            misses: 0,
            hb_seq: 0,
            suppress_blocked: false,
            escape_travel: 0,
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

    /// Update whether the connected peer is trusted for input. The daemon starts
    /// runtimes only for trusted peers; this hook keeps the pure core's injection gate
    /// explicit and testable.
    pub fn set_peer_trusted(&mut self, trusted: bool) {
        self.input_auth.set_peer_trusted(trusted);
    }

    /// Update whether local capability/permission allows injected peer input.
    pub fn set_input_allowed(&mut self, allowed: bool) {
        self.input_auth.set_input_allowed(allowed);
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
        vec![
            Action::SetCursorVisible(self.is_owner()),
            Action::SetCaptureMode(self.capture_mode()),
        ]
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
            LocalInputEvent::CursorMoved { x, y, dx, dy, .. } => self.on_cursor(x, y, dx, dy, owns),
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

    fn on_cursor(&mut self, x: i32, y: i32, dx: i32, dy: i32, owns: bool) -> Vec<Action> {
        if owns {
            // On our own screen: cross to the peer when motion pushes past the configured
            // edge (predictive — see `crosses_out`; the clamped absolute position alone never
            // reaches the wall). Don't cross if we already learned this session that we can't
            // suppress local input (would cross-and-reclaim in a loop while resting on the edge).
            let was_cross_out_armed = self.cross_out_armed;
            if !was_cross_out_armed && self.local_left_cross_edge(x, y) {
                self.cross_out_armed = true;
                return vec![Action::Capture(CaptureDecision::PassThrough)];
            }
            if was_cross_out_armed && !self.suppress_blocked && self.crosses_out(x, y, dx, dy) {
                return self.cross_to_peer(x, y);
            }
            return vec![Action::Capture(CaptureDecision::PassThrough)];
        }
        // We own the peer: drive it with the TRUE relative device delta (dx, dy) for this
        // event. Relative motion keeps flowing even when our local cursor is parked at the
        // screen edge / suppressed — the old absolute-position delta (x - prev_x) froze at
        // the edge (dx == 0), pinning the peer cursor near the entry point.
        if self.local_escape_back(x, y, dx, dy) {
            return self.reclaim_local();
        }
        // Emergency escape, independent of peer geometry and the (possibly frozen) local
        // absolute position: accumulate net home-ward true-device travel and reclaim once a
        // full local screen-width of deliberate push has been demanded. Clamp at 0 so motion
        // into the peer cancels progress rather than driving it negative.
        self.escape_travel = (self.escape_travel + self.homeward_delta(dx, dy)).max(0);
        if self.escape_travel >= self.escape_threshold() {
            return self.reclaim_local();
        }
        // `saturating_add`: dx/dy originate at untrusted capture/FFI boundaries, so a
        // pathological delta must clamp rather than overflow-panic (debug) or wrap (release).
        self.peer_x = clamp(
            self.peer_x.saturating_add(dx),
            0,
            self.layout.peer_width.saturating_sub(1).max(0),
        );
        self.peer_y = clamp(
            self.peer_y.saturating_add(dy),
            0,
            self.layout.peer_height.saturating_sub(1).max(0),
        );
        if !self.reclaim_armed && self.peer_left_entry_edge() {
            self.reclaim_armed = true;
        }
        // Crossing back: the peer cursor hit the near edge moving toward us. The
        // reclaim is disarmed immediately after handoff so a first-sample bounce at
        // the seeded entry edge cannot instantly return ownership.
        if self.reclaim_armed && self.crosses_back(dx, dy) {
            // Our own visible cursor drifted across this screen while we drove the peer
            // (suppression stops apps seeing the motion, but the OS still moves the
            // pointer). Snap it back to the shared boundary so the return is continuous —
            // the cursor re-enters where the peer screen meets ours, then tracks inward —
            // instead of reappearing wherever it happened to drift to.
            let warp = self.entry_edge_warp(x, y);
            let mut actions = self.reclaim_local();
            actions.push(warp);
            return actions;
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

    /// Does the motion at `(x, y)` with true device delta `(dx, dy)` cross the edge the peer
    /// sits on? PREDICTIVE: the OS clamps the on-screen cursor ~1px inside the display, so the
    /// post-clamp absolute position never reliably reaches the exact boundary even while the
    /// device keeps emitting motion into the wall — testing `x >= width-1` alone then never
    /// fires ("the cursor won't leave"). Adding the delta so the *predicted* next position
    /// (`x + dx`) reaching the bound triggers the cross, the way lan-mouse does, fixes that
    /// while staying compatible with reaching the bound directly. (A cursor resting at the
    /// clamped edge with `dx == 0` sits just inside the bound and won't cross; the
    /// grant-once / `suppress_blocked` guards in `on_cursor` handle an exactly-pinned cursor
    /// as before.)
    fn crosses_out(&self, x: i32, y: i32, dx: i32, dy: i32) -> bool {
        let max_x = self.layout.width.saturating_sub(1).max(0);
        let max_y = self.layout.height.saturating_sub(1).max(0);
        match self.layout.edge {
            Edge::Right => {
                x >= max_x.saturating_sub(EDGE_REARM_MARGIN) && x.saturating_add(dx) >= max_x
            }
            Edge::Left => x <= EDGE_REARM_MARGIN && x.saturating_add(dx) <= 0,
            Edge::Bottom => {
                y >= max_y.saturating_sub(EDGE_REARM_MARGIN) && y.saturating_add(dy) >= max_y
            }
            Edge::Top => y <= EDGE_REARM_MARGIN && y.saturating_add(dy) <= 0,
        }
    }

    fn local_left_cross_edge(&self, x: i32, y: i32) -> bool {
        let max_x = self.layout.width.saturating_sub(1).max(0);
        let max_y = self.layout.height.saturating_sub(1).max(0);
        match self.layout.edge {
            Edge::Right => x < max_x.saturating_sub(EDGE_REARM_MARGIN),
            Edge::Left => x > EDGE_REARM_MARGIN,
            Edge::Bottom => y < max_y.saturating_sub(EDGE_REARM_MARGIN),
            Edge::Top => y > EDGE_REARM_MARGIN,
        }
    }

    fn local_escape_back(&self, x: i32, y: i32, dx: i32, dy: i32) -> bool {
        let max_x = self.layout.width.saturating_sub(1).max(0);
        let max_y = self.layout.height.saturating_sub(1).max(0);
        match self.layout.edge {
            Edge::Right => x <= EDGE_REARM_MARGIN && dx < 0,
            Edge::Left => x >= max_x.saturating_sub(EDGE_REARM_MARGIN) && dx > 0,
            Edge::Bottom => y <= EDGE_REARM_MARGIN && dy < 0,
            Edge::Top => y >= max_y.saturating_sub(EDGE_REARM_MARGIN) && dy > 0,
        }
    }

    /// Warp our local cursor to the shared boundary on `layout.edge`, carrying the off-axis
    /// position so the return lands where the pointer currently sits on the other axis. For a
    /// left edge that is `x = 0` at the current `y`; the cursor then tracks inward (rightward)
    /// onto our screen, the way crossing between two physical monitors feels.
    fn entry_edge_warp(&self, x: i32, y: i32) -> Action {
        let max_x = self.layout.width.saturating_sub(1).max(0);
        let max_y = self.layout.height.saturating_sub(1).max(0);
        let (wx, wy) = match self.layout.edge {
            Edge::Left => (0, clamp(y, 0, max_y)),
            Edge::Right => (max_x, clamp(y, 0, max_y)),
            Edge::Top => (clamp(x, 0, max_x), 0),
            Edge::Bottom => (clamp(x, 0, max_x), max_y),
        };
        Action::Inject(Inject::MoveCursor {
            display_id: 0,
            x: wx,
            y: wy,
        })
    }

    /// Signed component of this event's true device delta pointing back toward our own
    /// screen (the direction that returns ownership across `layout.edge`). Mirrors the axis
    /// and sign `crosses_back` tests, but on the raw delta rather than the clamped peer
    /// cursor, so it stays meaningful even when `peer_x`/`peer_y` is pinned at a bound.
    fn homeward_delta(&self, dx: i32, dy: i32) -> i32 {
        match self.layout.edge {
            Edge::Left => dx,
            Edge::Right => -dx,
            Edge::Bottom => -dy,
            Edge::Top => dy,
        }
    }

    /// Home-ward travel that forces an emergency reclaim: one local screen-width (horizontal
    /// edges) or screen-height (vertical edges). `.max(1)` keeps a zero/unknown layout from
    /// arming the escape on the first sample.
    fn escape_threshold(&self) -> i32 {
        // Floor so a degenerate/unknown tiny layout can't arm the escape on a few stray
        // pixels; on any real display `width`/`height` (≫ the floor) is what applies.
        const ESCAPE_MIN_TRAVEL: i32 = 64;
        match self.layout.edge {
            Edge::Left | Edge::Right => self.layout.width.max(ESCAPE_MIN_TRAVEL),
            Edge::Top | Edge::Bottom => self.layout.height.max(ESCAPE_MIN_TRAVEL),
        }
    }

    /// Has the peer cursor returned to the near edge (back toward us)?
    fn crosses_back(&self, dx: i32, dy: i32) -> bool {
        match self.layout.edge {
            // Horizontal edges track the x delta; vertical edges the y delta.
            Edge::Right => self.peer_x <= 0 && dx < 0,
            Edge::Left => self.peer_x >= self.layout.peer_width.saturating_sub(1) && dx > 0,
            Edge::Bottom => self.peer_y <= 0 && dy < 0,
            Edge::Top => self.peer_y >= self.layout.peer_height.saturating_sub(1) && dy > 0,
        }
    }

    fn peer_left_entry_edge(&self) -> bool {
        let max_x = self.layout.peer_width.saturating_sub(1).max(0);
        let max_y = self.layout.peer_height.saturating_sub(1).max(0);
        match self.layout.edge {
            Edge::Right => self.peer_x > 0,
            Edge::Left => self.peer_x < max_x,
            Edge::Bottom => self.peer_y > 0,
            Edge::Top => self.peer_y < max_y,
        }
    }

    fn cross_to_peer(&mut self, x: i32, y: i32) -> Vec<Action> {
        let Some(epoch) = self.ownership.grant_to(self.peer) else {
            return vec![Action::Capture(CaptureDecision::PassThrough)];
        };
        self.reset_out();
        self.guard = ReplayGuard::default();
        self.held_keys.clear();
        self.input_auth.revoke_epoch();
        self.pending_ack = Some(PendingAck::new(epoch));
        // Seed the peer cursor at the entry edge: the crossing axis is pinned to the entry
        // side, the other axis carries over the local position so the cursor appears where
        // it left. (Horizontal edges cross on x → carry y; vertical edges cross on y →
        // carry x.)
        let max_x = self.layout.peer_width.saturating_sub(1).max(0);
        let max_y = self.layout.peer_height.saturating_sub(1).max(0);
        match self.layout.edge {
            Edge::Right => {
                self.peer_x = 0;
                self.peer_y = clamp(y, 0, max_y);
            }
            Edge::Left => {
                self.peer_x = max_x;
                self.peer_y = clamp(y, 0, max_y);
            }
            Edge::Bottom => {
                self.peer_y = 0;
                self.peer_x = clamp(x, 0, max_x);
            }
            Edge::Top => {
                self.peer_y = max_y;
                self.peer_x = clamp(x, 0, max_x);
            }
        }
        self.reclaim_armed = false;
        self.escape_travel = 0;
        let transfer = encode(&OwnershipTransfer {
            to: self.peer.to_vec(),
            owner_epoch: epoch,
            layout_rev: 0,
            reason: TransferReason::EdgeCross,
        });
        // SetCaptureMode goes first: the runtime escalates to ActiveForward (installs
        // suppressing hooks) before we start suppressing/forwarding this crossing.
        vec![
            Action::SetCaptureMode(CaptureMode::ActiveForward),
            Action::SendControl(TYPE_OWNERSHIP_TRANSFER, transfer),
            Action::SetCursorVisible(false),
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
        self.held_keys.clear();
        self.input_auth.revoke_epoch();
        self.pending_ack = None;
        self.reclaim_armed = true;
        self.cross_out_armed = false;
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
            Action::SetCursorVisible(true),
            Action::OwnerChanged { owner: me, epoch },
            Action::Capture(CaptureDecision::PassThrough),
        ]
    }

    /// The platform reported it cannot suppress local input (e.g. macOS Accessibility /
    /// Input Monitoring not granted, so the event tap is listen-only). We must NOT keep
    /// driving the peer while local input also reaches our own desktop — that delivers
    /// input to BOTH machines. Reclaim local control so the cursor stays put on this
    /// machine; the UI surfaces the missing permission so the user can grant it.
    pub fn on_suppress_unavailable(&mut self) -> Vec<Action> {
        if self.role != Role::Source {
            return Vec::new();
        }
        // Stop crossing for the rest of the session so we don't loop cross→reclaim while the
        // cursor rests on the edge. A fresh session (after the user grants the permission and
        // reconnects) starts with this clear.
        self.suppress_blocked = true;
        if self.is_owner() {
            // Already local — nothing to reclaim, just block further crossing.
            return Vec::new();
        }
        self.reclaim_local()
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
        self.input_rate.refill();
        if let Some(mut pending) = self.pending_ack {
            if pending.ticks_left == 0 {
                actions.extend(self.reclaim_local());
                return actions;
            }
            pending.ticks_left = pending.ticks_left.saturating_sub(1);
            self.pending_ack = Some(pending);
        }
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

fn clamp(v: i32, lo: i32, hi: i32) -> i32 {
    v.max(lo).min(hi.max(lo))
}
