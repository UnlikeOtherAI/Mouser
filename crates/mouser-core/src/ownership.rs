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
//! wins** and the loser adopts the winner's state at that epoch. Crucially this
//! equal-epoch tiebreak applies **only when both the local current-epoch ownership
//! and the incoming claim are reclaim-origin**: a normal owner-minted grant must
//! never be displaced by another claim at the same epoch. See [`Ownership::observe`].

use mouser_protocol::{FocusKind, TransferReason};

use crate::DeviceId;

/// Why an observed ownership claim was rejected.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RejectReason {
    /// The claimed epoch is older than or equal to the locally-known epoch and is not
    /// a winning simultaneous-reclaim tiebreak — i.e. stale (spec §7.4 acceptance rule).
    StaleEpoch,
    /// A simultaneous reclaim at the *same* epoch where our current owner has the
    /// lower (winning) `device_id`, so the incoming equal-epoch claim loses the
    /// tiebreak and is dropped. Only ever returned when both sides are reclaims.
    LostTiebreak,
    /// An equal-epoch claim that is **not** a proven simultaneous-reclaim race —
    /// either our current ownership or the incoming claim (or both) is a normal
    /// owner-minted grant. A grant is never displaced at the same epoch, so the
    /// claim is dropped without applying the lower-`device_id` tiebreak.
    EqualEpochNotReclaim,
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
    /// Whether the **current** `(owner, epoch)` originated from a *reclaim*
    /// (self-`reclaim()` or an accepted reclaim claim) rather than a normal
    /// owner-minted grant. Gates the equal-epoch simultaneous-reclaim tiebreak in
    /// [`Ownership::observe`]: a grant (`false`) is never displaced at the same epoch.
    reclaim_origin: bool,
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
            // Boot self-ownership is not a contested reclaim; epoch 0 can never be in
            // an equal-epoch reclaim race (real claims are epoch >= 1) anyway.
            reclaim_origin: false,
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
        // A grant is owner-minted, not a reclaim: it must never lose/win the
        // equal-epoch reclaim tiebreak.
        self.reclaim_origin = false;
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
        // This epoch is reclaim-origin: it may participate in the equal-epoch
        // simultaneous-reclaim tiebreak against another reclaim.
        self.reclaim_origin = true;
        new_epoch
    }

    /// Apply an **observed** ownership claim from the wire (`OwnershipTransfer` or
    /// `FocusState`), enforcing the spec §7.4 acceptance rule. `is_reclaim` is whether
    /// the incoming claim is a *self-reclaim* (wire `TransferReason::LocalReclaim`, or
    /// a `FocusState` whose owner self-minted on heartbeat-timeout) rather than a
    /// normal owner-minted grant; pass it through from the wire (see
    /// [`Ownership::observe_with_reason`]).
    ///
    /// - Accept iff `epoch` is **strictly greater** than the local epoch.
    /// - Equal-epoch **simultaneous reclaim** tiebreak: applied **only when our
    ///   current epoch is itself reclaim-origin AND the incoming claim is a reclaim**
    ///   — the proven simultaneous self-reclaim race. There the claim with the lower
    ///   `owner` `device_id` wins; if the incoming owner's id is lower, adopt it,
    ///   otherwise reject as [`RejectReason::LostTiebreak`]. If either side is a
    ///   normal grant, the equal-epoch claim is rejected as
    ///   [`RejectReason::EqualEpochNotReclaim`] — a granted owner is never displaced
    ///   at the same epoch by a stray/duplicate/malicious claim.
    /// - Otherwise reject as [`RejectReason::StaleEpoch`].
    ///
    /// On acceptance the local focus is recomputed: [`FocusKind::Active`] if we are
    /// the new owner, else [`FocusKind::Standby`].
    pub fn observe(&mut self, owner: DeviceId, epoch: u64, is_reclaim: bool) -> OwnershipUpdate {
        if epoch > self.epoch {
            return self.adopt(owner, epoch, is_reclaim);
        }
        if epoch == self.epoch && owner != self.owner {
            // The lower-device_id tiebreak is a *reclaim-vs-reclaim* resolver only.
            // Apply it iff BOTH our current epoch is reclaim-origin AND the incoming
            // claim is a reclaim. Otherwise a normal grant is in play and must stand.
            if !(self.reclaim_origin && is_reclaim) {
                return OwnershipUpdate::Rejected(RejectReason::EqualEpochNotReclaim);
            }
            if owner < self.owner {
                return self.adopt(owner, epoch, is_reclaim);
            }
            return OwnershipUpdate::Rejected(RejectReason::LostTiebreak);
        }
        OwnershipUpdate::Rejected(RejectReason::StaleEpoch)
    }

    /// [`Ownership::observe`] keyed on the wire `reason`: `TransferReason::LocalReclaim`
    /// is a reclaim, every other reason is a normal grant. Use this from the engine to
    /// feed an `OwnershipTransfer.reason` / inferred `FocusState` reason straight in.
    pub fn observe_with_reason(
        &mut self,
        owner: DeviceId,
        epoch: u64,
        reason: TransferReason,
    ) -> OwnershipUpdate {
        self.observe(owner, epoch, reason == TransferReason::LocalReclaim)
    }

    fn adopt(&mut self, owner: DeviceId, epoch: u64, is_reclaim: bool) -> OwnershipUpdate {
        self.owner = owner;
        self.epoch = epoch;
        self.focus = if owner == self.me {
            FocusKind::Active
        } else {
            FocusKind::Standby
        };
        // Record whether the accepted claim was a reclaim, so a later equal-epoch
        // reclaim race against this state is gated correctly.
        self.reclaim_origin = is_reclaim;
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
