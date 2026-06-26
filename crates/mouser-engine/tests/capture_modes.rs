//! Capture-mode and cursor-visibility lifecycle tests.

use mouser_core::platform::{CaptureMode, LocalInputEvent};
use mouser_engine::core::{Action, CaptureDecision, EngineCore};
use mouser_engine::{Edge, EdgeLayout};
use mouser_protocol::{
    to_cbor, OwnershipAck, OwnershipTransfer, PointerMotion, TransferReason, TYPE_OWNERSHIP_ACK,
    TYPE_OWNERSHIP_TRANSFER,
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

fn has_cursor_visible(actions: &[Action], want: bool) -> bool {
    actions
        .iter()
        .any(|a| matches!(a, Action::SetCursorVisible(visible) if *visible == want))
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

fn motion_of(actions: &[Action]) -> Option<PointerMotion> {
    actions.iter().find_map(|a| match a {
        Action::SendMotion(m) => Some(*m),
        _ => None,
    })
}

#[test]
fn source_starts_in_passive_edge_sensing() {
    let e = EngineCore::new_source(ME, PEER, EdgeLayout::side_by_side(100, 100, 100, 100));
    assert_eq!(e.capture_mode(), CaptureMode::PassiveEdge);
    assert!(
        has_set_mode(&e.initial_actions(), CaptureMode::PassiveEdge),
        "a source comes up in passive edge sensing, not forwarding capture"
    );
    assert!(has_cursor_visible(&e.initial_actions(), true));
}

#[test]
fn target_starts_and_stays_capture_off() {
    let mut t = EngineCore::new_target(ME, PEER);
    assert_eq!(t.capture_mode(), CaptureMode::Off);
    assert!(has_set_mode(&t.initial_actions(), CaptureMode::Off));
    assert!(has_cursor_visible(&t.initial_actions(), true));

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
fn ownership_changes_drive_cursor_visibility() {
    let mut source = EngineCore::new_source(ME, PEER, EdgeLayout::side_by_side(100, 100, 100, 100));
    let cross = source.on_local_input(cursor(99, 40));
    assert!(
        has_cursor_visible(&cross, false),
        "a source hides its local cursor while the peer owns input"
    );
    let rejected = source.on_control(TYPE_OWNERSHIP_ACK, &ownership_ack(1, false));
    assert!(
        has_cursor_visible(&rejected, true),
        "reclaiming local ownership shows the cursor again"
    );

    let mut target = EngineCore::new_target(ME, PEER);
    let grant = to_cbor(&OwnershipTransfer {
        to: ME.to_vec(),
        owner_epoch: 1,
        layout_rev: 0,
        reason: TransferReason::EdgeCross,
    })
    .unwrap();
    let accepted = target.on_control(TYPE_OWNERSHIP_TRANSFER, &grant);
    assert!(
        has_cursor_visible(&accepted, true),
        "the target shows its cursor when it becomes the owner"
    );

    let reclaim = to_cbor(&OwnershipTransfer {
        to: PEER.to_vec(),
        owner_epoch: 2,
        layout_rev: 0,
        reason: TransferReason::LocalReclaim,
    })
    .unwrap();
    let returned = target.on_control(TYPE_OWNERSHIP_TRANSFER, &reclaim);
    assert!(
        has_cursor_visible(&returned, false),
        "the target hides its cursor when ownership returns to the peer"
    );
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
    e.on_local_input(cursor(99, 40));
    e.on_control(TYPE_OWNERSHIP_ACK, &ownership_ack(1, true));
    assert_eq!(e.capture_mode(), CaptureMode::ActiveForward);

    let a = e.on_local_input(cursor_rel(98, 40, 5, 0));
    assert!(
        !has_set_mode(&a, CaptureMode::PassiveEdge),
        "moving into the peer arms reclaim but keeps forwarding"
    );
    assert!(has_capture(&a, CaptureDecision::Suppress));
    assert!(!e.is_owner());

    let a = e.on_local_input(cursor_rel(98, 40, -6, 0));
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
fn immediate_bounce_at_left_entry_edge_does_not_reclaim() {
    let mut e = EngineCore::new_source(
        ME,
        PEER,
        EdgeLayout::with_edge(100, 100, 100, 100, Edge::Left),
    );
    e.on_local_input(cursor_rel(1, 40, -6, 0));
    e.on_control(TYPE_OWNERSHIP_ACK, &ownership_ack(1, true));
    assert!(!e.is_owner());

    let bounced = e.on_local_input(cursor_rel(1, 40, 8, 0));
    let m = motion_of(&bounced).expect("entry-edge bounce is forwarded, not reclaimed");
    assert_eq!(m.x, 99);
    assert!(
        !has_set_mode(&bounced, CaptureMode::PassiveEdge),
        "a delta back toward the source while still pinned at the entry edge is ignored"
    );
    assert!(has_capture(&bounced, CaptureDecision::Suppress));
    assert!(!e.is_owner());

    let away = e.on_local_input(cursor_rel(1, 40, -2, 0));
    let m = motion_of(&away).expect("moving into the peer arms reclaim");
    assert_eq!(m.x, 97);
    assert!(!e.is_owner());

    let reclaim = e.on_local_input(cursor_rel(1, 40, 3, 0));
    assert!(
        has_set_mode(&reclaim, CaptureMode::PassiveEdge),
        "once the peer cursor left the entry edge, returning to it reclaims"
    );
    assert!(e.is_owner());
}

#[test]
fn left_edge_reclaim_must_move_inside_before_crossing_out_again() {
    let mut e = EngineCore::new_source(
        ME,
        PEER,
        EdgeLayout::with_edge(100, 100, 100, 100, Edge::Left),
    );
    e.on_local_input(cursor_rel(1, 40, -6, 0));
    e.on_control(TYPE_OWNERSHIP_ACK, &ownership_ack(1, true));
    assert!(!e.is_owner());

    let away = e.on_local_input(cursor_rel(1, 40, -10, 0));
    assert_eq!(motion_of(&away).map(|m| m.x), Some(89));

    let reclaim = e.on_local_input(cursor_rel(1, 40, 20, 0));
    assert!(
        has_set_mode(&reclaim, CaptureMode::PassiveEdge),
        "returning across the Windows right edge reclaims Mac ownership"
    );
    assert!(e.is_owner());

    let immediate_recross = e.on_local_input(cursor_rel(1, 40, -20, 0));
    assert!(
        has_control(&immediate_recross, TYPE_OWNERSHIP_TRANSFER).is_none(),
        "a restored Mac cursor still at the left edge must not immediately hand off again"
    );
    assert!(has_capture(
        &immediate_recross,
        CaptureDecision::PassThrough
    ));
    assert!(e.is_owner());

    let moved_inside = e.on_local_input(cursor_rel(12, 40, -200, 0));
    assert!(
        has_control(&moved_inside, TYPE_OWNERSHIP_TRANSFER).is_none(),
        "the event that moves back inside may re-arm crossing, but must not also cross"
    );
    assert!(has_capture(&moved_inside, CaptureDecision::PassThrough));
    assert!(e.is_owner());

    let huge_delta_from_inside = e.on_local_input(cursor_rel(50, 40, -200, 0));
    assert!(
        has_control(&huge_delta_from_inside, TYPE_OWNERSHIP_TRANSFER).is_none(),
        "a synthetic warp-sized delta from the middle of the Mac screen must not cross"
    );
    assert!(e.is_owner());

    let recross = e.on_local_input(cursor_rel(1, 40, -20, 0));
    assert!(
        has_control(&recross, TYPE_OWNERSHIP_TRANSFER).is_some(),
        "after the local cursor moves back inside the Mac screen, the left edge can cross again"
    );
    assert!(!e.is_owner());
}

#[test]
fn heartbeat_timeout_reclaim_drops_to_passive_edge() {
    let mut e = EngineCore::new_source(ME, PEER, EdgeLayout::side_by_side(100, 100, 100, 100));
    e.on_local_input(cursor(99, 40));
    e.on_control(TYPE_OWNERSHIP_ACK, &ownership_ack(1, true));
    assert_eq!(e.capture_mode(), CaptureMode::ActiveForward);

    e.on_tick();
    e.on_tick();
    let a = e.on_tick();
    assert!(e.is_owner());
    assert!(
        has_set_mode(&a, CaptureMode::PassiveEdge),
        "a heartbeat-timeout reclaim tears down forwarding capture"
    );
    assert_eq!(e.capture_mode(), CaptureMode::PassiveEdge);
}

#[test]
fn relative_nudge_off_entry_edge_survives_a_leftward_delta() {
    const SEED_STEP: i32 = 16;
    let mut e = EngineCore::new_source(ME, PEER, EdgeLayout::side_by_side(1, 100, 100, 100));
    e.on_local_input(cursor(0, 50));
    e.on_control(TYPE_OWNERSHIP_ACK, &ownership_ack(1, true));
    assert!(!e.is_owner(), "peer owns after the edge cross");

    let nudged = e.on_local_input(cursor_rel(SEED_STEP, 50, SEED_STEP, 0));
    assert!(!e.is_owner(), "the nudge keeps the peer in control");
    assert_eq!(motion_of(&nudged).map(|m| m.x), Some(SEED_STEP));

    let left = e.on_local_input(cursor_rel(SEED_STEP - 1, 50, -1, 0));
    assert!(
        !e.is_owner(),
        "a leftward delta after the nudge must not instantly reclaim"
    );
    assert_eq!(motion_of(&left).map(|m| m.x), Some(SEED_STEP - 1));
    assert_eq!(e.capture_mode(), CaptureMode::ActiveForward);
}

#[test]
fn bottom_edge_crosses_seeds_top_and_traverses_then_reclaims() {
    let mut e = EngineCore::new_source(
        ME,
        PEER,
        EdgeLayout::with_edge(100, 100, 100, 100, Edge::Bottom),
    );
    assert!(e.is_owner());
    assert!(has_capture(
        &e.on_local_input(cursor(40, 50)),
        CaptureDecision::PassThrough
    ));

    let a = e.on_local_input(cursor(40, 99));
    assert!(
        has_control(&a, TYPE_OWNERSHIP_TRANSFER).is_some(),
        "crosses at the bottom edge"
    );
    assert!(!e.is_owner());
    e.on_control(TYPE_OWNERSHIP_ACK, &ownership_ack(1, true));

    let a = e.on_local_input(cursor_rel(40, 99, 0, 30));
    let m = motion_of(&a).expect("motion forwarded while owning the peer");
    assert!(
        m.y > 0,
        "peer cursor moved down from the top entry, got y={}",
        m.y
    );
    assert!(has_capture(&a, CaptureDecision::Suppress));

    let a = e.on_local_input(cursor_rel(40, 99, 0, -50));
    assert!(
        has_set_mode(&a, CaptureMode::PassiveEdge),
        "crossing back up reclaims to passive edge"
    );
    assert!(e.is_owner());
}

#[test]
fn left_edge_crosses_seeds_right_and_traverses_then_reclaims() {
    let mut e = EngineCore::new_source(
        ME,
        PEER,
        EdgeLayout::with_edge(100, 100, 100, 100, Edge::Left),
    );
    assert!(e.is_owner());
    assert!(has_capture(
        &e.on_local_input(cursor(40, 50)),
        CaptureDecision::PassThrough
    ));

    let a = e.on_local_input(cursor_rel(1, 40, -6, 0));
    assert!(
        matches!(
            a.first(),
            Some(Action::SetCaptureMode(CaptureMode::ActiveForward))
        ),
        "left-edge crossing escalates to ActiveForward before suppressing/forwarding"
    );
    assert!(
        has_control(&a, TYPE_OWNERSHIP_TRANSFER).is_some(),
        "predictive cross fires at the left edge"
    );
    assert!(has_capture(&a, CaptureDecision::Suppress));
    assert!(!e.is_owner());

    let a = e.on_control(TYPE_OWNERSHIP_ACK, &ownership_ack(1, true));
    let m = motion_of(&a).expect("ACK seeds peer motion");
    assert_eq!(m.x, 99, "left-edge entry starts at peer right edge");
    assert_eq!(m.y, 40);

    let a = e.on_local_input(cursor_rel(1, 40, -10, 0));
    let m = motion_of(&a).expect("motion forwarded while owning the peer");
    assert_eq!(m.x, 89);
    assert_eq!(m.y, 40);
    assert!(has_capture(&a, CaptureDecision::Suppress));
    assert!(!e.is_owner());

    let a = e.on_local_input(cursor_rel(1, 40, 5, 0));
    let m = motion_of(&a).expect("motion forwarded before returning to the edge");
    assert_eq!(m.x, 94);
    assert_eq!(m.y, 40);
    assert!(
        !has_set_mode(&a, CaptureMode::PassiveEdge),
        "moving right before reaching the peer entry edge should not reclaim"
    );
    assert!(has_capture(&a, CaptureDecision::Suppress));
    assert!(!e.is_owner());

    let a = e.on_local_input(cursor_rel(1, 40, 10, 0));
    assert!(
        has_set_mode(&a, CaptureMode::PassiveEdge),
        "crossing back right at the peer entry edge reclaims to passive edge"
    );
    assert!(e.is_owner());
}
