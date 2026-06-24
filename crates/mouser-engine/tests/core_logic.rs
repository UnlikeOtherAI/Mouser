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

fn cursor(x: i32, y: i32) -> LocalInputEvent {
    LocalInputEvent::CursorMoved {
        display_id: 0,
        x,
        y,
        dx: 0,
        dy: 0,
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
fn predictive_edge_cross_fires_when_clamped_just_inside_the_wall() {
    // The macOS source "wouldn't let the cursor out": the OS clamps the on-screen cursor ~1px
    // inside the display, so the absolute x never reaches width-1 (=99) even while the device
    // keeps emitting motion into the wall. The predictive test (x + dx) must still cross,
    // driven by the device delta — the OLD absolute test (x >= 99) would NOT fire here.
    let mut e = EngineCore::new_source(ME, PEER, EdgeLayout::side_by_side(100, 100, 100, 100));
    assert!(e.is_owner());

    let a = e.on_local_input(cursor_rel(98, 40, 6, 0));
    let transfer = has_control(&a, TYPE_OWNERSHIP_TRANSFER)
        .expect("predictive cross fires from the device delta even though x is clamped inside");
    let t: OwnershipTransfer = from_cbor(&transfer).expect("decode");
    assert_eq!(t.to, PEER.to_vec());
    assert_eq!(t.reason, TransferReason::EdgeCross);
    assert!(!e.is_owner());
    assert_eq!(e.owner(), PEER);
}

#[test]
fn cursor_resting_clamped_inside_the_wall_does_not_cross() {
    // The flip side: a cursor resting at the clamped edge (x=98, no device motion) must NOT
    // cross — only motion whose predicted position reaches the bound does. Guards against a
    // loop-cross while the cursor merely rests against the wall.
    let mut e = EngineCore::new_source(ME, PEER, EdgeLayout::side_by_side(100, 100, 100, 100));
    let a = e.on_local_input(cursor_rel(98, 40, 0, 0));
    assert!(
        has_control(&a, TYPE_OWNERSHIP_TRANSFER).is_none(),
        "resting one pixel inside the wall with no delta must not cross"
    );
    assert!(e.is_owner());
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
fn exhausted_repeat_cap_never_drops_key_release() {
    let mut t = EngineCore::new_target(ME, PEER);
    let grant = to_cbor(&OwnershipTransfer {
        to: ME.to_vec(),
        owner_epoch: 1,
        layout_rev: 0,
        reason: TransferReason::EdgeCross,
    })
    .unwrap();
    t.on_control(TYPE_OWNERSHIP_TRANSFER, &grant);

    for ctr in 1..=260 {
        let repeat = to_cbor(&KeyEvent {
            usage: 0x07,
            down: true,
            mods: 0,
            owner_epoch: 1,
            ctr,
        })
        .unwrap();
        let _ = t.on_control(TYPE_KEY_EVENT, &repeat);
    }

    let release = to_cbor(&KeyEvent {
        usage: 0x07,
        down: false,
        mods: 0,
        owner_epoch: 1,
        ctr: 261,
    })
    .unwrap();
    assert_eq!(
        t.on_control(TYPE_KEY_EVENT, &release),
        vec![Action::Inject(Inject::Key {
            usage: 0x07,
            down: false,
            mods: 0,
        })],
        "key-up is a transition, so it must not be shed with auto-repeat key-downs"
    );
}

#[test]
fn motion_burst_does_not_starve_key_transitions() {
    let mut t = EngineCore::new_target(ME, PEER);
    let grant = to_cbor(&OwnershipTransfer {
        to: ME.to_vec(),
        owner_epoch: 1,
        layout_rev: 0,
        reason: TransferReason::EdgeCross,
    })
    .unwrap();
    t.on_control(TYPE_OWNERSHIP_TRANSFER, &grant);

    for seq in 1..=500 {
        let _ = t.on_motion(Datagram::Motion(PointerMotion {
            owner_epoch: 1,
            seq,
            display_id: 0,
            x: 10,
            y: 20,
        }));
    }

    let key = to_cbor(&KeyEvent {
        usage: 0x04,
        down: true,
        mods: 0,
        owner_epoch: 1,
        ctr: 1,
    })
    .unwrap();
    assert_eq!(
        t.on_control(TYPE_KEY_EVENT, &key),
        vec![Action::Inject(Inject::Key {
            usage: 0x04,
            down: true,
            mods: 0,
        })],
        "lossy pointer motion must not spend the key/button repeat bucket"
    );
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
