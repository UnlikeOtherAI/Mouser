//! Input-ownership state machine — the `owner_epoch` token (spec §7.4).
//!
//! Exactly one device owns input at a time, modeled as a single token with a
//! **monotonic `owner_epoch`**. This module is the pure decision core: it holds no
//! clock, sends nothing, and only computes "given my current view and this event,
//! what is the new ownership state and what should I emit?". The engine wires the
//! results onto the wire (`OwnershipTransfer`, `OwnershipAck`, `FocusState`).
//!
//! ## Minting rules (spec §7.4)
//! - **Normal handoff:** only the *current owner* mints `epoch+1` — an owner-minted
//!   `OwnershipTransfer` grant. A non-owner that wants ownership sends an
//!   `OwnershipRequest`; the owner mints and grants. See [`Ownership::grant_to`].
//! - **Reclaim:** when the owner is unreachable (heartbeat-timeout) or on a local
//!   input / panic reclaim, the reclaiming device *self-mints* `epoch+1` directly.
//!   See [`Ownership::reclaim`].
//!
//! ## Acceptance rule (spec §7.4)
//! Accept an incoming `OwnershipTransfer`/`FocusState` **iff** its `owner_epoch` is
//! *strictly greater* than the locally-known epoch. The lone exception is a
//! **simultaneous reclaim**: two devices independently self-mint the same `epoch+1`
//! after an owner heartbeat-timeout — there the claim with the **lower `device_id`
//! wins** and the loser adopts the winner's state at that epoch. See
//! [`Ownership::observe`].

use mouser_protocol::FocusKind;

use crate::DeviceId;

/// Why an observed ownership claim was rejected.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RejectReason {
    /// The claimed epoch is older than or equal to the locally-known epoch and is not
    /// a winning simultaneous-reclaim tiebreak — i.e. stale (spec §7.4 acceptance rule).
    StaleEpoch,
    /// A simultaneous reclaim at the *same* epoch where our current owner has the
    /// lower (winning) `device_id`, so the incoming equal-epoch claim loses the
    /// tiebreak and is dropped.
    LostTiebreak,
}

/// What the local view should do in response to an observed claim.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OwnershipUpdate {
    /// The claim was accepted; local state moved to `(owner, epoch)`.
    Accepted {
        /// The new owner.
        owner: DeviceId,
        /// The new (strictly greater, or tiebreak-winning) epoch.
        epoch: u64,
    },
    /// The claim was rejected; local state is unchanged.
    Rejected(RejectReason),
}

/// The local view of input ownership: the current owner, the current `owner_epoch`,
/// and this device's focus state.
///
/// One instance per engine. All transitions are total functions of the current
/// state plus the event; nothing here reads a clock or performs I/O.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ownership {
    /// This device's own `device_id` (used for self-mint and tiebreaks).
    me: DeviceId,
    /// The currently-known owner.
    owner: DeviceId,
    /// The currently-known `owner_epoch` (monotonic).
    epoch: u64,
    /// This device's focus state (spec §7.4 `FocusKind`).
    focus: FocusKind,
}

impl Ownership {
    /// Create the initial view at boot: `me` owns input at `epoch = 0` and is
    /// [`FocusKind::Active`]. A device that has not yet joined a cluster considers
    /// itself the owner of its own input.
    pub fn new(me: DeviceId) -> Self {
        Self {
            me,
            owner: me,
            epoch: 0,
            focus: FocusKind::Active,
        }
    }

    /// This device's own `device_id`.
    pub fn me(&self) -> DeviceId {
        self.me
    }

    /// The currently-known owner.
    pub fn owner(&self) -> DeviceId {
        self.owner
    }

    /// The currently-known `owner_epoch`.
    pub fn epoch(&self) -> u64 {
        self.epoch
    }

    /// This device's focus state.
    pub fn focus(&self) -> FocusKind {
        self.focus
    }

    /// Whether this device currently owns input.
    pub fn is_owner(&self) -> bool {
        self.owner == self.me
    }

    /// **Normal handoff** (spec §7.4): the *current owner* mints `epoch+1` and grants
    /// ownership to `target`. Returns the minted `OwnershipTransfer` epoch to put on
    /// the wire, or `None` if this device is **not** the current owner (a non-owner
    /// must instead send an `OwnershipRequest` and let the owner mint).
    ///
    /// On success the local view advances to the new epoch with `target` as owner and
    /// this device moves to [`FocusKind::Standby`] (it gave input away). The grant
    /// still requires an `OwnershipAck` from the target before input is forwarded;
    /// tracking that ack is the engine's job.
    pub fn grant_to(&mut self, target: DeviceId) -> Option<u64> {
        if !self.is_owner() {
            return None;
        }
        let new_epoch = self.epoch.saturating_add(1);
        self.owner = target;
        self.epoch = new_epoch;
        self.focus = if target == self.me {
            FocusKind::Active
        } else {
            FocusKind::Standby
        };
        Some(new_epoch)
    }

    /// **Reclaim** (spec §7.4): self-mint `epoch+1` and take ownership directly. Used
    /// when the owner is unreachable (heartbeat-timeout) or on a local-input / panic
    /// reclaim — there is no reachable owner to mint a grant. Returns the minted
    /// epoch. This device becomes the owner and goes [`FocusKind::Active`].
    ///
    /// Two devices reclaiming at once both mint the same `epoch+1`; the
    /// simultaneous-reclaim tiebreak in [`Ownership::observe`] resolves the race.
    pub fn reclaim(&mut self) -> u64 {
        let new_epoch = self.epoch.saturating_add(1);
        self.owner = self.me;
        self.epoch = new_epoch;
        self.focus = FocusKind::Active;
        new_epoch
    }

    /// Apply an **observed** ownership claim from the wire (`OwnershipTransfer` or
    /// `FocusState`), enforcing the spec §7.4 acceptance rule:
    ///
    /// - Accept iff `epoch` is **strictly greater** than the local epoch.
    /// - Equal-epoch **simultaneous reclaim** tiebreak: the claim with the lower
    ///   `owner` `device_id` wins; if the incoming owner's id is lower than the
    ///   current owner's, adopt it; otherwise reject as [`RejectReason::LostTiebreak`].
    /// - Otherwise reject as [`RejectReason::StaleEpoch`].
    ///
    /// On acceptance the local focus is recomputed: [`FocusKind::Active`] if we are
    /// the new owner, else [`FocusKind::Standby`].
    pub fn observe(&mut self, owner: DeviceId, epoch: u64) -> OwnershipUpdate {
        if epoch > self.epoch {
            return self.adopt(owner, epoch);
        }
        if epoch == self.epoch && owner != self.owner {
            // Simultaneous reclaim at the same epoch: lower device_id wins.
            if owner < self.owner {
                return self.adopt(owner, epoch);
            }
            return OwnershipUpdate::Rejected(RejectReason::LostTiebreak);
        }
        OwnershipUpdate::Rejected(RejectReason::StaleEpoch)
    }

    fn adopt(&mut self, owner: DeviceId, epoch: u64) -> OwnershipUpdate {
        self.owner = owner;
        self.epoch = epoch;
        self.focus = if owner == self.me {
            FocusKind::Active
        } else {
            FocusKind::Standby
        };
        OwnershipUpdate::Accepted { owner, epoch }
    }

    /// Mark input as blocked (secure desktop, lock screen, missing permission —
    /// spec §7.4 `CapabilityState`). The engine returns ownership to the source; this
    /// device's local focus becomes [`FocusKind::InputBlocked`]. Does not change the
    /// epoch or owner — that is driven by the returning `OwnershipTransfer`.
    pub fn mark_input_blocked(&mut self) {
        self.focus = FocusKind::InputBlocked;
    }

    /// Mark this device disconnected from the owner (heartbeat-timeout before any
    /// reclaim decision — spec §7.4 / architecture §7).
    pub fn mark_disconnected(&mut self) {
        self.focus = FocusKind::Disconnected;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let _ = own.observe(A, 5); // A owns at epoch 5
        assert_eq!(own.owner(), A);
        assert_eq!(own.grant_to(C), None, "non-owner must not mint");
        assert_eq!(own.owner(), A, "state unchanged on refused grant");
        assert_eq!(own.epoch(), 5);
    }

    #[test]
    fn two_owner_race_prevented_by_strictly_greater_rule() {
        // Local view: B owns at epoch 5. A stale grant at the SAME epoch from a
        // different owner that is NOT lower must be rejected, preventing two valid
        // owners at one epoch.
        let mut own = Ownership::new(C);
        assert!(matches!(
            own.observe(B, 5),
            OwnershipUpdate::Accepted { .. }
        ));
        assert_eq!(own.owner(), B);
        // Equal epoch, owner C is higher than B -> reject (no second owner at 5).
        match own.observe(C, 5) {
            OwnershipUpdate::Rejected(RejectReason::LostTiebreak) => {}
            other => panic!("expected LostTiebreak, got {other:?}"),
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
            own.observe(A, 6),
            OwnershipUpdate::Accepted { .. }
        ));
        assert_eq!(own.owner(), A);
        assert_eq!(own.epoch(), 6);
        // LOW also self-minted epoch 6; LOW < A -> LOW wins, we adopt it.
        match own.observe(LOW, 6) {
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
            own.observe(LOW, 6),
            OwnershipUpdate::Accepted { .. }
        ));
        match own.observe(A, 6) {
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
        match own.observe(A, 1) {
            OwnershipUpdate::Rejected(RejectReason::LostTiebreak) => {}
            other => panic!("expected LostTiebreak, got {other:?}"),
        }
        assert_eq!(own.owner(), LOW);
    }

    #[test]
    fn stale_epoch_is_rejected() {
        let mut own = Ownership::new(C);
        assert!(matches!(
            own.observe(A, 10),
            OwnershipUpdate::Accepted { .. }
        ));
        // A lower epoch from anyone is stale.
        match own.observe(B, 9) {
            OwnershipUpdate::Rejected(RejectReason::StaleEpoch) => {}
            other => panic!("expected StaleEpoch, got {other:?}"),
        }
        // The same epoch from the SAME owner is also stale (not strictly greater).
        match own.observe(A, 10) {
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
            own.observe(A, 1),
            OwnershipUpdate::Accepted { .. }
        ));
        // Even a higher device_id wins with a strictly-greater epoch.
        match own.observe(A, 2) {
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
        match own.observe(C, 3) {
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
        match b.observe(B, e1) {
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
        match a.observe(A, e2) {
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
}
