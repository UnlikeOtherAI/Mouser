//! Unit tests for the pure [`EngineCore`] state machine: edge-crossing handoff,
//! input forwarding, injection, anti-replay, and heartbeat-timeout reclaim.

use mouser_core::platform::{CaptureMode, LocalInputEvent};
use mouser_engine::core::{Action, CaptureDecision, EngineCore, Inject};
use mouser_engine::EdgeLayout;
use mouser_protocol::{
    from_cbor, to_cbor, Datagram, KeyEvent, OwnershipAck, OwnershipTransfer, PointerButton,
    PointerMotion, TransferReason, TYPE_KEY_EVENT, TYPE_OWNERSHIP_ACK, TYPE_OWNERSHIP_TRANSFER,
    TYPE_POINTER_BUTTON,
};

const ME: [u8; 32] = [1u8; 32];
const PEER: [u8; 32] = [2u8; 32];

fn has_control(actions: &[Action], ty: u16) -> Option<Vec<u8>> {
    actions.iter().find_map(|a| match a {
        Action::SendControl(t, p) if *t == ty => Some(p.clone()),
        _ => None,
    })
}

fn has_capture(actions: &[Action], want: CaptureDecision) -> bool {
    actions
        .iter()
        .any(|a| matches!(a, Action::Capture(d) if *d == want))
}

fn has_set_mode(actions: &[Action], want: CaptureMode) -> bool {
    actions
        .iter()
        .any(|a| matches!(a, Action::SetCaptureMode(m) if *m == want))
}

fn has_any_set_mode(actions: &[Action]) -> bool {
    actions
        .iter()
        .any(|a| matches!(a, Action::SetCaptureMode(_)))
}

fn cursor(x: i32, y: i32) -> LocalInputEvent {
    LocalInputEvent::CursorMoved {
        display_id: 0,
        x,
        y,
        dx: 0,
        dy: 0,
    }
}

/// Cursor event carrying a relative device delta (the engine drives a controlled peer from
/// `dx`/`dy`, which keep flowing while the local cursor is parked at the edge).
fn cursor_rel(x: i32, y: i32, dx: i32, dy: i32) -> LocalInputEvent {
    LocalInputEvent::CursorMoved {
        display_id: 0,
        x,
        y,
        dx,
        dy,
    }
}

fn ownership_ack(epoch: u64, accepted: bool) -> Vec<u8> {
    to_cbor(&OwnershipAck {
        owner_epoch: epoch,
        accepted,
        reason: None,
    })
    .unwrap_or_default()
}

#[test]
fn source_passes_through_until_edge_then_grants_to_peer() {
    let mut e = EngineCore::new_source(ME, PEER, EdgeLayout::side_by_side(100, 100, 100, 100));
    assert!(e.is_owner());

    // Inside our own screen: pass through, still owner.
    let a = e.on_local_input(cursor(50, 50));
    assert!(has_capture(&a, CaptureDecision::PassThrough));
    assert!(e.is_owner());

    // Reach the right edge: grant to peer, suppress locally, emit OwnershipTransfer.
    let a = e.on_local_input(cursor(99, 40));
    let transfer = has_control(&a, TYPE_OWNERSHIP_TRANSFER).expect("OwnershipTransfer sent");
    let t: OwnershipTransfer = from_cbor(&transfer).expect("decode");
    assert_eq!(t.to, PEER.to_vec());
    assert_eq!(t.owner_epoch, 1);
    assert_eq!(t.reason, TransferReason::EdgeCross);
    assert!(has_capture(&a, CaptureDecision::Suppress));
    assert!(!e.is_owner());
    assert_eq!(e.owner(), PEER);
}

#[test]
fn source_forwards_input_with_incrementing_counters_while_peer_owns() {
    let mut e = EngineCore::new_source(ME, PEER, EdgeLayout::side_by_side(100, 100, 100, 100));
    e.on_local_input(cursor(99, 40)); // cross → peer owns, counters reset
    e.on_control(TYPE_OWNERSHIP_ACK, &ownership_ack(1, true));

    let a = e.on_local_input(LocalInputEvent::Key {
        usage: 0x04,
        down: true,
        mods: 0,
    });
    let k1: KeyEvent =
        from_cbor(&has_control(&a, TYPE_KEY_EVENT).expect("key forwarded")).expect("decode");
    assert_eq!(
        (k1.usage, k1.down, k1.owner_epoch, k1.ctr),
        (0x04, true, 1, 1)
    );
    assert!(has_capture(&a, CaptureDecision::Suppress));

    let a = e.on_local_input(LocalInputEvent::Button {
        button: 0,
        down: true,
    });
    let b: PointerButton =
        from_cbor(&has_control(&a, TYPE_POINTER_BUTTON).expect("button forwarded")).expect("dec");
    assert_eq!(
        b.ctr, 2,
        "counter strictly increments across forwarded events"
    );
}

#[test]
fn target_accepts_grant_then_injects_with_anti_replay() {
    let mut t = EngineCore::new_target(ME, PEER);

    // Peer (source) grants input to us at epoch 1.
    let grant = to_cbor(&OwnershipTransfer {
        to: ME.to_vec(),
        owner_epoch: 1,
        layout_rev: 0,
        reason: TransferReason::EdgeCross,
    })
    .unwrap();
    let a = t.on_control(TYPE_OWNERSHIP_TRANSFER, &grant);
    assert!(
        has_control(&a, TYPE_OWNERSHIP_ACK).is_some(),
        "acks the transfer"
    );
    assert!(t.is_owner());
    assert_eq!(t.epoch(), 1);

    // A key for the current epoch is injected.
    let key = |ctr: u64| {
        to_cbor(&KeyEvent {
            usage: 0x07,
            down: true,
            mods: 0,
            owner_epoch: 1,
            ctr,
        })
        .unwrap()
    };
    let a = t.on_control(TYPE_KEY_EVENT, &key(1));
    assert_eq!(
        a,
        vec![Action::Inject(Inject::Key {
            usage: 0x07,
            down: true,
            mods: 0
        })]
    );

    // Replay of the same counter is rejected (anti-replay §7.5).
    assert!(
        t.on_control(TYPE_KEY_EVENT, &key(1)).is_empty(),
        "replayed ctr dropped"
    );
    // A lower counter is rejected.
    assert!(
        t.on_control(TYPE_KEY_EVENT, &key(0)).is_empty(),
        "non-increasing ctr dropped"
    );
    // A higher counter is accepted.
    assert_eq!(
        t.on_control(TYPE_KEY_EVENT, &key(2)),
        vec![Action::Inject(Inject::Key {
            usage: 0x07,
            down: true,
            mods: 0
        })]
    );
}

#[test]
fn target_drops_input_before_current_epoch_grant() {
    let mut t = EngineCore::new_target(ME, PEER);
    let key = to_cbor(&KeyEvent {
        usage: 0x07,
        down: true,
        mods: 0,
        owner_epoch: 0,
        ctr: 1,
    })
    .unwrap();

    assert!(
        t.on_control(TYPE_KEY_EVENT, &key).is_empty(),
        "a target must not inject peer input before accepting an ownership grant"
    );
}

#[test]
fn source_reclaims_when_ownership_ack_deadline_expires() {
    let mut e = EngineCore::new_source(ME, PEER, EdgeLayout::side_by_side(100, 100, 100, 100));
    e.on_local_input(cursor(99, 40));
    assert!(!e.is_owner());

    e.on_tick();
    e.on_tick();
    let actions = e.on_tick();
    assert!(e.is_owner(), "lost ACK returns ownership locally");
    assert!(has_set_mode(&actions, CaptureMode::PassiveEdge));
}

#[test]
fn negative_ownership_ack_reclaims_local_input() {
    let mut e = EngineCore::new_source(ME, PEER, EdgeLayout::side_by_side(100, 100, 100, 100));
    e.on_local_input(cursor(99, 40));
    let ack = to_cbor(&OwnershipAck {
        owner_epoch: 1,
        accepted: false,
        reason: Some("blocked".to_string()),
    })
    .unwrap();

    let actions = e.on_control(TYPE_OWNERSHIP_ACK, &ack);
    assert!(e.is_owner());
    assert!(has_set_mode(&actions, CaptureMode::PassiveEdge));
}

#[test]
fn input_rate_cap_drops_excess_peer_events() {
    let mut t = EngineCore::new_target(ME, PEER);
    let grant = to_cbor(&OwnershipTransfer {
        to: ME.to_vec(),
        owner_epoch: 1,
        layout_rev: 0,
        reason: TransferReason::EdgeCross,
    })
    .unwrap();
    t.on_control(TYPE_OWNERSHIP_TRANSFER, &grant);

    let mut injected = 0usize;
    for ctr in 1..=260 {
        let key = to_cbor(&KeyEvent {
            usage: 0x07,
            down: true,
            mods: 0,
            owner_epoch: 1,
            ctr,
        })
        .unwrap();
        if !t.on_control(TYPE_KEY_EVENT, &key).is_empty() {
            injected = injected.saturating_add(1);
        }
    }
    assert_eq!(injected, 240, "single-peer burst cap is enforced");
}

#[test]
fn source_capable_peer_accepts_grant_and_injects() {
    let mut peer = EngineCore::new_source(ME, PEER, EdgeLayout::side_by_side(100, 100, 100, 100));

    let grant = to_cbor(&OwnershipTransfer {
        to: ME.to_vec(),
        owner_epoch: 1,
        layout_rev: 0,
        reason: TransferReason::EdgeCross,
    })
    .unwrap();
    let a = peer.on_control(TYPE_OWNERSHIP_TRANSFER, &grant);
    assert!(
        has_control(&a, TYPE_OWNERSHIP_ACK).is_some(),
        "acks the transfer"
    );
    assert!(peer.is_owner());
    assert_eq!(peer.epoch(), 1);

    let key = to_cbor(&KeyEvent {
        usage: 0x07,
        down: true,
        mods: 0,
        owner_epoch: 1,
        ctr: 1,
    })
    .unwrap();
    assert_eq!(
        peer.on_control(TYPE_KEY_EVENT, &key),
        vec![Action::Inject(Inject::Key {
            usage: 0x07,
            down: true,
            mods: 0,
        })]
    );

    let local = peer.on_local_input(LocalInputEvent::Key {
        usage: 0x04,
        down: true,
        mods: 0,
    });
    assert!(has_capture(&local, CaptureDecision::PassThrough));
}

#[test]
fn target_rejects_events_for_a_stale_epoch() {
    let mut t = EngineCore::new_target(ME, PEER);
    let grant = to_cbor(&OwnershipTransfer {
        to: ME.to_vec(),
        owner_epoch: 2,
        layout_rev: 0,
        reason: TransferReason::EdgeCross,
    })
    .unwrap();
    t.on_control(TYPE_OWNERSHIP_TRANSFER, &grant);
    assert_eq!(t.epoch(), 2);

    // An event tagged with an old epoch must be dropped.
    let stale = to_cbor(&KeyEvent {
        usage: 5,
        down: true,
        mods: 0,
        owner_epoch: 1,
        ctr: 99,
    })
    .unwrap();
    assert!(t.on_control(TYPE_KEY_EVENT, &stale).is_empty());
}

#[test]
fn target_injects_motion_and_drops_out_of_order_seq() {
    let mut t = EngineCore::new_target(ME, PEER);
    let grant = to_cbor(&OwnershipTransfer {
        to: ME.to_vec(),
        owner_epoch: 1,
        layout_rev: 0,
        reason: TransferReason::EdgeCross,
    })
    .unwrap();
    t.on_control(TYPE_OWNERSHIP_TRANSFER, &grant);

    let motion = |seq: u32, x: i32| {
        Datagram::Motion(PointerMotion {
            owner_epoch: 1,
            seq,
            display_id: 0,
            x,
            y: 20,
        })
    };
    assert_eq!(
        t.on_motion(motion(5, 10)),
        vec![Action::Inject(Inject::MoveCursor {
            display_id: 0,
            x: 10,
            y: 20
        })]
    );
    // Older/equal seq is dropped (keep-newest, §7.6 anti-replay).
    assert!(t.on_motion(motion(5, 11)).is_empty());
    assert!(t.on_motion(motion(4, 12)).is_empty());
    // Newer seq is applied.
    assert_eq!(
        t.on_motion(motion(6, 30)),
        vec![Action::Inject(Inject::MoveCursor {
            display_id: 0,
            x: 30,
            y: 20
        })]
    );
}

#[test]
fn source_reclaims_after_heartbeat_timeout() {
    let mut e = EngineCore::new_source(ME, PEER, EdgeLayout::side_by_side(100, 100, 100, 100));
    e.on_local_input(cursor(99, 40)); // hand off to peer
    e.on_control(TYPE_OWNERSHIP_ACK, &ownership_ack(1, true));
    assert!(!e.is_owner());

    // Three silent ticks (no peer heartbeat) → reclaim local input.
    e.on_tick();
    e.on_tick();
    let a = e.on_tick();
    assert!(e.is_owner(), "source reclaims after 3 missed heartbeats");
    let transfer = has_control(&a, TYPE_OWNERSHIP_TRANSFER).expect("reclaim transfer");
    let t: OwnershipTransfer = from_cbor(&transfer).unwrap();
    assert_eq!(t.reason, TransferReason::LocalReclaim);
    assert_eq!(t.to, ME.to_vec());
}

#[test]
fn peer_heartbeat_prevents_reclaim() {
    let mut e = EngineCore::new_source(ME, PEER, EdgeLayout::side_by_side(100, 100, 100, 100));
    e.on_local_input(cursor(99, 40));
    e.on_control(TYPE_OWNERSHIP_ACK, &ownership_ack(1, true));
    // Heartbeat arrives between ticks, resetting the miss counter each time.
    for _ in 0..6 {
        e.on_tick();
        let hb = to_cbor(&mouser_protocol::Heartbeat { seq: 1 }).unwrap();
        e.on_control(mouser_protocol::TYPE_HEARTBEAT, &hb);
    }
    assert!(!e.is_owner(), "a live peer keeps ownership");
}

// --- Capture-mode lifecycle (edge sensing is not input forwarding) ---

#[test]
fn source_starts_in_passive_edge_sensing() {
    let e = EngineCore::new_source(ME, PEER, EdgeLayout::side_by_side(100, 100, 100, 100));
    assert_eq!(e.capture_mode(), CaptureMode::PassiveEdge);
    assert!(
        has_set_mode(&e.initial_actions(), CaptureMode::PassiveEdge),
        "a source comes up in passive edge sensing, not forwarding capture"
    );
}

#[test]
fn target_starts_and_stays_capture_off() {
    let mut t = EngineCore::new_target(ME, PEER);
    assert_eq!(t.capture_mode(), CaptureMode::Off);
    assert!(has_set_mode(&t.initial_actions(), CaptureMode::Off));

    // Accepting a grant makes the target the owner, but it still never captures.
    let grant = to_cbor(&OwnershipTransfer {
        to: ME.to_vec(),
        owner_epoch: 1,
        layout_rev: 0,
        reason: TransferReason::EdgeCross,
    })
    .unwrap();
    let a = t.on_control(TYPE_OWNERSHIP_TRANSFER, &grant);
    assert!(t.is_owner());
    assert!(
        !has_any_set_mode(&a),
        "a target never emits a capture-mode change"
    );
    assert_eq!(t.capture_mode(), CaptureMode::Off);
}

#[test]
fn idle_cursor_on_own_screen_does_not_escalate_capture() {
    let mut e = EngineCore::new_source(ME, PEER, EdgeLayout::side_by_side(100, 100, 100, 100));
    let a = e.on_local_input(cursor(50, 50));
    assert!(
        !has_any_set_mode(&a),
        "a cursor that has not reached the edge keeps passive sensing"
    );
    assert_eq!(e.capture_mode(), CaptureMode::PassiveEdge);
}

#[test]
fn edge_cross_escalates_to_active_forward_first() {
    let mut e = EngineCore::new_source(ME, PEER, EdgeLayout::side_by_side(100, 100, 100, 100));
    let a = e.on_local_input(cursor(99, 40));
    assert!(
        matches!(
            a.first(),
            Some(Action::SetCaptureMode(CaptureMode::ActiveForward))
        ),
        "the capture escalates to ActiveForward before suppressing/forwarding the crossing"
    );
    assert_eq!(e.capture_mode(), CaptureMode::ActiveForward);
    assert!(!e.is_owner());
}

#[test]
fn reclaim_by_crossing_back_drops_to_passive_edge() {
    let mut e = EngineCore::new_source(ME, PEER, EdgeLayout::side_by_side(100, 100, 100, 100));
    e.on_local_input(cursor(99, 40)); // cross to peer → ActiveForward
    e.on_control(TYPE_OWNERSHIP_ACK, &ownership_ack(1, true));
    assert_eq!(e.capture_mode(), CaptureMode::ActiveForward);

    // Move the (suppressed) cursor back across the near edge → reclaim locally. The peer
    // cursor was seeded at the entry edge (peer_x == 0); a leftward delta crosses back.
    let a = e.on_local_input(cursor_rel(98, 40, -1, 0));
    assert!(
        matches!(
            a.first(),
            Some(Action::SetCaptureMode(CaptureMode::PassiveEdge))
        ),
        "reclaim drops suppressing capture back to passive edge sensing first"
    );
    assert!(e.is_owner());
    assert_eq!(e.capture_mode(), CaptureMode::PassiveEdge);
}

#[test]
fn heartbeat_timeout_reclaim_drops_to_passive_edge() {
    let mut e = EngineCore::new_source(ME, PEER, EdgeLayout::side_by_side(100, 100, 100, 100));
    e.on_local_input(cursor(99, 40)); // hand off → ActiveForward
    e.on_control(TYPE_OWNERSHIP_ACK, &ownership_ack(1, true));
    assert_eq!(e.capture_mode(), CaptureMode::ActiveForward);

    e.on_tick();
    e.on_tick();
    let a = e.on_tick(); // third missed heartbeat → reclaim
    assert!(e.is_owner());
    assert!(
        has_set_mode(&a, CaptureMode::PassiveEdge),
        "a heartbeat-timeout reclaim tears down forwarding capture"
    );
    assert_eq!(e.capture_mode(), CaptureMode::PassiveEdge);
}
