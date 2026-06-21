//! Integration tests for the `owner_epoch` ownership state machine (spec §7.4).
//! These live outside `src/ownership.rs` to keep that module under the 500-line
//! source budget while preserving full coverage. They exercise the public API only.

use mouser_core::{DeviceId, Ownership, OwnershipUpdate, RejectReason};
use mouser_protocol::{FocusKind, TransferReason};

const A: DeviceId = [0xAA; 32];
const B: DeviceId = [0xBB; 32];
const C: DeviceId = [0xCC; 32];
/// A device_id strictly lower than `A` for tiebreak tests.
const LOW: DeviceId = [0x01; 32];

#[test]
fn boots_as_self_owner_active() {
    let own = Ownership::new(A);
    assert!(own.is_owner());
    assert_eq!(own.owner(), A);
    assert_eq!(own.epoch(), 0);
    assert_eq!(own.focus(), FocusKind::Active);
}

#[test]
fn normal_handoff_owner_mints_and_grants() {
    // A is the owner; A grants to B. Only the owner can mint.
    let mut own = Ownership::new(A);
    let minted = own.grant_to(B).expect("owner mints a grant");
    assert_eq!(minted, 1);
    assert_eq!(own.owner(), B);
    assert_eq!(own.epoch(), 1);
    assert_eq!(own.focus(), FocusKind::Standby);
}

#[test]
fn non_owner_cannot_mint_a_grant() {
    // B is not the owner (A is): grant_to must refuse, state unchanged.
    let mut own = Ownership::new(B);
    let _ = own.observe(A, 5, false); // A owns at epoch 5 (granted)
    assert_eq!(own.owner(), A);
    assert_eq!(own.grant_to(C), None, "non-owner must not mint");
    assert_eq!(own.owner(), A, "state unchanged on refused grant");
    assert_eq!(own.epoch(), 5);
}

#[test]
fn two_owner_race_prevented_by_strictly_greater_rule() {
    // Local view: B owns at epoch 5 via a normal grant. A stale claim at the SAME
    // epoch from a different owner must be rejected, preventing two valid owners
    // at one epoch. Because B's epoch is a grant (not reclaim), the lower-id
    // tiebreak never even applies.
    let mut own = Ownership::new(C);
    assert!(matches!(
        own.observe(B, 5, false),
        OwnershipUpdate::Accepted { .. }
    ));
    assert_eq!(own.owner(), B);
    // Equal epoch against a granted owner -> rejected without tiebreak.
    match own.observe(C, 5, false) {
        OwnershipUpdate::Rejected(RejectReason::EqualEpochNotReclaim) => {}
        other => panic!("expected EqualEpochNotReclaim, got {other:?}"),
    }
    assert_eq!(own.owner(), B, "owner unchanged");
    assert_eq!(own.epoch(), 5);
}

#[test]
fn simultaneous_reclaim_lower_device_id_wins() {
    // After an owner timeout, two devices self-mint the same epoch+1.
    // Local view currently has owner A at epoch 5 (the timed-out owner replaced
    // by a self-mint we observed from A's successor). Both LOW and A reclaim to 6.
    let mut own = Ownership::new(C);
    assert!(matches!(
        own.observe(A, 6, true),
        OwnershipUpdate::Accepted { .. }
    ));
    assert_eq!(own.owner(), A);
    assert_eq!(own.epoch(), 6);
    // LOW also self-minted (reclaimed) epoch 6; LOW < A -> LOW wins, we adopt it.
    match own.observe(LOW, 6, true) {
        OwnershipUpdate::Accepted { owner, epoch } => {
            assert_eq!(owner, LOW);
            assert_eq!(epoch, 6);
        }
        other => panic!("expected Accepted(LOW,6), got {other:?}"),
    }
    assert_eq!(own.owner(), LOW);
    assert_eq!(own.epoch(), 6);
}

#[test]
fn simultaneous_reclaim_higher_device_id_loses() {
    // Mirror of the above: we already hold LOW at epoch 6; a higher-id claim at
    // the same epoch must lose the tiebreak.
    let mut own = Ownership::new(C);
    assert!(matches!(
        own.observe(LOW, 6, true),
        OwnershipUpdate::Accepted { .. }
    ));
    match own.observe(A, 6, true) {
        OwnershipUpdate::Rejected(RejectReason::LostTiebreak) => {}
        other => panic!("expected LostTiebreak, got {other:?}"),
    }
    assert_eq!(own.owner(), LOW, "lower id retains ownership");
}

#[test]
fn self_reclaim_wins_tiebreak_when_we_are_lower() {
    // We are LOW and self-mint a reclaim; a competing equal-epoch claim from a
    // higher id A must be rejected so our reclaim stands.
    let mut own = Ownership::new(LOW);
    let minted = own.reclaim();
    assert_eq!(minted, 1);
    assert_eq!(own.owner(), LOW);
    assert_eq!(own.focus(), FocusKind::Active);
    match own.observe(A, 1, true) {
        OwnershipUpdate::Rejected(RejectReason::LostTiebreak) => {}
        other => panic!("expected LostTiebreak, got {other:?}"),
    }
    assert_eq!(own.owner(), LOW);
}

#[test]
fn stale_epoch_is_rejected() {
    let mut own = Ownership::new(C);
    assert!(matches!(
        own.observe(A, 10, false),
        OwnershipUpdate::Accepted { .. }
    ));
    // A lower epoch from anyone is stale.
    match own.observe(B, 9, false) {
        OwnershipUpdate::Rejected(RejectReason::StaleEpoch) => {}
        other => panic!("expected StaleEpoch, got {other:?}"),
    }
    // The same epoch from the SAME owner is also stale (not strictly greater).
    match own.observe(A, 10, false) {
        OwnershipUpdate::Rejected(RejectReason::StaleEpoch) => {}
        other => panic!("expected StaleEpoch, got {other:?}"),
    }
    assert_eq!(own.owner(), A);
    assert_eq!(own.epoch(), 10);
}

#[test]
fn strictly_greater_epoch_always_accepted() {
    let mut own = Ownership::new(C);
    assert!(matches!(
        own.observe(A, 1, false),
        OwnershipUpdate::Accepted { .. }
    ));
    // Even a higher device_id wins with a strictly-greater epoch.
    match own.observe(A, 2, false) {
        OwnershipUpdate::Accepted { owner, epoch } => {
            assert_eq!(owner, A);
            assert_eq!(epoch, 2);
        }
        other => panic!("expected Accepted, got {other:?}"),
    }
}

#[test]
fn focus_active_when_we_become_owner() {
    let mut own = Ownership::new(C);
    // Observe a grant TO us.
    match own.observe(C, 3, false) {
        OwnershipUpdate::Accepted { .. } => {}
        other => panic!("expected Accepted, got {other:?}"),
    }
    assert!(own.is_owner());
    assert_eq!(own.focus(), FocusKind::Active);
}

#[test]
fn grant_to_self_keeps_active() {
    // Owner re-mints to itself (e.g. local reclaim while already owner).
    let mut own = Ownership::new(A);
    let minted = own.grant_to(A).expect("owner mints");
    assert_eq!(minted, 1);
    assert!(own.is_owner());
    assert_eq!(own.focus(), FocusKind::Active);
}

#[test]
fn input_blocked_and_disconnected_transitions() {
    let mut own = Ownership::new(A);
    own.mark_input_blocked();
    assert_eq!(own.focus(), FocusKind::InputBlocked);
    own.mark_disconnected();
    assert_eq!(own.focus(), FocusKind::Disconnected);
}

#[test]
fn full_handoff_round_trip() {
    // A grants to B; from B's perspective it observes and becomes owner; then B
    // grants back to A.
    let mut a = Ownership::new(A);
    let mut b = Ownership::new(B);

    let e1 = a.grant_to(B).expect("A mints");
    assert_eq!(e1, 1);
    // B observes A's grant.
    match b.observe(B, e1, false) {
        OwnershipUpdate::Accepted { owner, epoch } => {
            assert_eq!(owner, B);
            assert_eq!(epoch, 1);
        }
        other => panic!("expected Accepted, got {other:?}"),
    }
    assert!(b.is_owner());

    // B grants back to A.
    let e2 = b.grant_to(A).expect("B mints");
    assert_eq!(e2, 2);
    match a.observe(A, e2, false) {
        OwnershipUpdate::Accepted { owner, epoch } => {
            assert_eq!(owner, A);
            assert_eq!(epoch, 2);
        }
        other => panic!("expected Accepted, got {other:?}"),
    }
    assert!(a.is_owner());
    assert_eq!(a.focus(), FocusKind::Active);
    assert_eq!(b.focus(), FocusKind::Standby);
}

#[test]
fn non_reclaim_equal_epoch_lower_id_does_not_displace_granted_owner() {
    // FIX 1: a legitimately GRANTED owner (A at epoch 6, not a reclaim) must NOT
    // be displaced by an equal-epoch claim from a lower device_id when that claim
    // is not a reclaim — that would let a stray/duplicate/malicious FocusState
    // steal ownership from the rightful grantee. LOW < A, yet the grant stands.
    let mut own = Ownership::new(C);
    assert!(matches!(
        own.observe(A, 6, false),
        OwnershipUpdate::Accepted { .. }
    ));
    match own.observe(LOW, 6, false) {
        OwnershipUpdate::Rejected(RejectReason::EqualEpochNotReclaim) => {}
        other => panic!("expected EqualEpochNotReclaim, got {other:?}"),
    }
    assert_eq!(own.owner(), A, "granted owner not displaced");
    assert_eq!(own.epoch(), 6);
}

#[test]
fn reclaim_claim_cannot_displace_granted_owner_at_same_epoch() {
    // Asymmetric guard: even a *reclaim* claim with a lower id must not displace a
    // current epoch that is a GRANT — the tiebreak needs BOTH sides to be reclaims.
    let mut own = Ownership::new(C);
    assert!(matches!(
        own.observe(A, 6, false), // A granted at 6
        OwnershipUpdate::Accepted { .. }
    ));
    match own.observe(LOW, 6, true) {
        OwnershipUpdate::Rejected(RejectReason::EqualEpochNotReclaim) => {}
        other => panic!("expected EqualEpochNotReclaim, got {other:?}"),
    }
    assert_eq!(own.owner(), A, "granted owner not displaced by a reclaim");
}

#[test]
fn grant_claim_cannot_displace_reclaim_owner_at_same_epoch() {
    // Mirror: current epoch is a reclaim (A self-minted 6) but the incoming
    // lower-id claim is a normal grant -> not a simultaneous reclaim, so reject.
    let mut own = Ownership::new(C);
    assert!(matches!(
        own.observe(A, 6, true), // A reclaimed 6
        OwnershipUpdate::Accepted { .. }
    ));
    match own.observe(LOW, 6, false) {
        OwnershipUpdate::Rejected(RejectReason::EqualEpochNotReclaim) => {}
        other => panic!("expected EqualEpochNotReclaim, got {other:?}"),
    }
    assert_eq!(own.owner(), A, "reclaim owner not displaced by a grant");
}

#[test]
fn grant_then_self_reclaim_origin_is_tracked() {
    // grant_to clears reclaim_origin even after a prior reclaim set it, so a later
    // equal-epoch reclaim race against a granted-away epoch is rejected.
    let mut own = Ownership::new(LOW);
    let _ = own.reclaim(); // epoch 1, reclaim-origin
                           // Now LOW (owner) grants to a higher id A at epoch 2: a grant, clears origin.
    let e = own.grant_to(A).expect("owner mints");
    assert_eq!(e, 2);
    // A competing reclaim at epoch 2 from an even-lower id must NOT displace the
    // grant (current epoch is a grant, not a reclaim).
    match own.observe([0x00; 32], 2, true) {
        OwnershipUpdate::Rejected(RejectReason::EqualEpochNotReclaim) => {}
        other => panic!("expected EqualEpochNotReclaim, got {other:?}"),
    }
    assert_eq!(own.owner(), A);
}

#[test]
fn duplicate_reclaim_observation_is_idempotent() {
    // R2: observing the SAME reclaim claim twice must be idempotent — the second copy
    // is at an epoch that is no longer strictly-greater, so it is rejected as stale and
    // leaves state untouched (a replayed/duplicated OwnershipTransfer can't re-apply).
    let mut own = Ownership::new(C);
    match own.observe(A, 6, true) {
        OwnershipUpdate::Accepted { owner, epoch } => {
            assert_eq!(owner, A);
            assert_eq!(epoch, 6);
        }
        other => panic!("expected Accepted, got {other:?}"),
    }
    // Exact duplicate of the same reclaim: equal owner + equal epoch -> not strictly
    // greater, and owner == current owner so the tiebreak branch is skipped -> stale.
    match own.observe(A, 6, true) {
        OwnershipUpdate::Rejected(RejectReason::StaleEpoch) => {}
        other => panic!("expected StaleEpoch on duplicate reclaim, got {other:?}"),
    }
    assert_eq!(own.owner(), A, "duplicate reclaim must not change owner");
    assert_eq!(own.epoch(), 6, "duplicate reclaim must not change epoch");
}

#[test]
fn self_reclaim_then_duplicate_self_observation_is_noop() {
    // R2: after we self-reclaim, observing our own already-applied reclaim epoch again
    // (e.g. an echoed FocusState) is a no-op stale reject, not a re-mint or refocus.
    let mut own = Ownership::new(LOW);
    let minted = own.reclaim();
    assert_eq!(minted, 1);
    assert!(own.is_owner());
    assert_eq!(own.focus(), FocusKind::Active);
    // Our own reclaim, echoed back at the same epoch: stale, state unchanged.
    match own.observe(LOW, 1, true) {
        OwnershipUpdate::Rejected(RejectReason::StaleEpoch) => {}
        other => panic!("expected StaleEpoch on self echo, got {other:?}"),
    }
    assert_eq!(own.owner(), LOW);
    assert_eq!(own.epoch(), 1);
    assert_eq!(own.focus(), FocusKind::Active);
}

#[test]
fn observe_with_reason_maps_local_reclaim_to_reclaim() {
    // observe_with_reason: LocalReclaim is a reclaim, others are grants.
    // Genuine simultaneous reclaim via the wire reason resolves by lower id.
    let mut own = Ownership::new(C);
    assert!(matches!(
        own.observe_with_reason(A, 6, TransferReason::LocalReclaim),
        OwnershipUpdate::Accepted { .. }
    ));
    match own.observe_with_reason(LOW, 6, TransferReason::LocalReclaim) {
        OwnershipUpdate::Accepted { owner, .. } => assert_eq!(owner, LOW),
        other => panic!("expected Accepted(LOW), got {other:?}"),
    }

    // A non-reclaim reason (EdgeCross) at equal epoch never wins the tiebreak.
    let mut own2 = Ownership::new(C);
    assert!(matches!(
        own2.observe_with_reason(A, 6, TransferReason::EdgeCross),
        OwnershipUpdate::Accepted { .. }
    ));
    match own2.observe_with_reason(LOW, 6, TransferReason::EdgeCross) {
        OwnershipUpdate::Rejected(RejectReason::EqualEpochNotReclaim) => {}
        other => panic!("expected EqualEpochNotReclaim, got {other:?}"),
    }
    assert_eq!(own2.owner(), A);
}
