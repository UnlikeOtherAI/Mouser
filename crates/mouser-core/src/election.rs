//! Lease-based coordinator election (spec §7.10).
//!
//! The coordinator is a cosmetic "who's in charge" label plus an optional
//! unattended-admission fallback — it serializes nothing in steady state
//! (architecture §4.5). Election is **lease-based on local-monotonic TTL**, never a
//! cross-machine wall clock, with Raft-style term rules.
//!
//! ## Time is injected
//! This module **never reads the OS clock**. Every method that needs "now" takes an
//! injected [`Instant`]; the caller (the engine, or a test) supplies a monotonic
//! value. `ttl_ms` is a *duration* (spec §7.10): on receiving a
//! `CoordinatorLease` a node sets `deadline = now + ttl_ms`; the holder renews at
//! `ttl/3`.
//!
//! ## Term rules (spec §7.10)
//! - A candidate **increments `term`** when it issues a `CoordinatorClaim`.
//! - A holder that sees a **strictly-higher `term` yields**.
//! - **Equal `term` → lowest `device_id` wins** (the sole deterministic tiebreak).
//!
//! These rules give a defined partition-heal: when two holders meet, the higher term
//! wins (equal terms break to the lower id).

use std::time::{Duration, Instant};

use crate::DeviceId;

/// Default lease TTL (spec §7.10: `ttl_ms = 6000`).
pub const DEFAULT_TTL: Duration = Duration::from_millis(6000);

/// Maximum lease TTL (spec §7.10: capped at 30000 ms).
pub const MAX_TTL: Duration = Duration::from_millis(30_000);

/// A coordinator lease as carried on the wire (`[80] CoordinatorLease`, spec §7.10).
///
/// `ttl` is a **duration**, not an absolute time — the receiver turns it into a local
/// deadline against its own monotonic clock.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Lease {
    /// The advertised coordinator.
    pub holder: DeviceId,
    /// The lease term (Raft-style; higher wins, equal breaks to lower id).
    pub term: u64,
    /// Time-to-live as a duration from receipt.
    pub ttl: Duration,
}

/// What the engine should do after feeding an event to the [`Election`] machine.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ElectionEvent {
    /// Nothing to do; local state already consistent with the input.
    None,
    /// Emit a `CoordinatorClaim { candidate, term }` (spec §7.10).
    Claim {
        /// The candidate (this device).
        candidate: DeviceId,
        /// The incremented term.
        term: u64,
    },
    /// Emit a `CoordinatorYield { from, term }` — this device yielded to a superior
    /// holder (spec §7.10).
    Yield {
        /// This device (the yielding former holder/candidate).
        from: DeviceId,
        /// The term at which it yielded.
        term: u64,
    },
    /// Emit/refresh a `CoordinatorLease { holder, term, ttl }` — this device is the
    /// holder and should advertise (renewal at `ttl/3`).
    RenewLease(Lease),
}

/// The currently-believed coordinator and the local deadline of its lease.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Held {
    holder: DeviceId,
    term: u64,
    ttl: Duration,
    /// Local-monotonic expiry, recomputed on each accepted lease/renew.
    deadline: Instant,
}

/// Lease-based coordinator election state machine (spec §7.10).
///
/// Construct with [`Election::new`]; drive it with [`Election::on_lease`],
/// [`Election::on_claim`], [`Election::on_yield`], [`Election::tick`], and
/// [`Election::start_claim`]. The machine holds no clock — pass `now` in.
#[derive(Clone, Copy, Debug)]
pub struct Election {
    me: DeviceId,
    /// The current lease, if any coordinator is believed to hold one.
    held: Option<Held>,
    /// The highest term this device has ever issued or observed (monotonic). A
    /// candidate increments from here.
    seen_term: u64,
}

impl Election {
    /// Create an election view for this device with no coordinator yet.
    pub fn new(me: DeviceId) -> Self {
        Self {
            me,
            held: None,
            seen_term: 0,
        }
    }

    /// This device's `device_id`.
    pub fn me(&self) -> DeviceId {
        self.me
    }

    /// The currently-believed coordinator, or `None` if there is none (no lease yet,
    /// or the last lease expired — call [`Election::tick`] to expire it first).
    pub fn coordinator(&self) -> Option<DeviceId> {
        self.held.map(|h| h.holder)
    }

    /// The current lease term, if a coordinator is held.
    pub fn term(&self) -> Option<u64> {
        self.held.map(|h| h.term)
    }

    /// Whether this device is the current coordinator.
    pub fn is_coordinator(&self) -> bool {
        self.coordinator() == Some(self.me)
    }

    /// The local deadline of the current lease, if any.
    pub fn deadline(&self) -> Option<Instant> {
        self.held.map(|h| h.deadline)
    }

    /// Clamp an advertised TTL to the spec's `MAX_TTL` cap (spec §7.10), treating a
    /// zero TTL as the default.
    fn clamp_ttl(ttl: Duration) -> Duration {
        if ttl.is_zero() {
            DEFAULT_TTL
        } else {
            ttl.min(MAX_TTL)
        }
    }

    /// Decide whether `(term, holder)` is superior to `(other_term, other_holder)`
    /// under the spec §7.10 term rules: higher term wins; equal term → lower id.
    fn supersedes(term: u64, holder: DeviceId, other_term: u64, other_holder: DeviceId) -> bool {
        if term != other_term {
            term > other_term
        } else {
            holder < other_holder
        }
    }

    /// Apply a received `CoordinatorLease` (spec §7.10). Accepts the lease iff it
    /// supersedes the current one (higher term, or equal term with a lower-or-equal
    /// holder id); on acceptance sets `deadline = now + ttl`.
    ///
    /// Returns [`ElectionEvent::Yield`] if this device was the holder and is now
    /// superseded by a superior lease, else [`ElectionEvent::None`].
    pub fn on_lease(&mut self, lease: Lease, now: Instant) -> ElectionEvent {
        let prior_seen = self.seen_term;
        self.seen_term = self.seen_term.max(lease.term);
        let ttl = Self::clamp_ttl(lease.ttl);

        match self.held {
            Some(current) => {
                if current.holder == lease.holder && lease.term >= current.term {
                    // Renewal (or re-advert) from the same holder: refresh deadline.
                    self.held = Some(Held {
                        holder: lease.holder,
                        term: lease.term.max(current.term),
                        ttl,
                        deadline: now + ttl,
                    });
                    return ElectionEvent::None;
                }
                if Self::supersedes(lease.term, lease.holder, current.term, current.holder) {
                    let was_me = current.holder == self.me;
                    self.held = Some(Held {
                        holder: lease.holder,
                        term: lease.term,
                        ttl,
                        deadline: now + ttl,
                    });
                    if was_me {
                        return ElectionEvent::Yield {
                            from: self.me,
                            term: lease.term,
                        };
                    }
                    return ElectionEvent::None;
                }
                // Inferior lease: ignore (the superior holder will keep advertising).
                ElectionEvent::None
            }
            None => {
                // No current coordinator. Accept the lease only if its term is not
                // *below* a term we've already seen: a lease whose term has been
                // superseded is stale/replayed and must not resurrect a coordinator
                // we (or the cluster) already moved past. Equal-to-seen is fine — that
                // is the live holder advertising at the current term.
                if lease.term < prior_seen {
                    return ElectionEvent::None;
                }
                self.held = Some(Held {
                    holder: lease.holder,
                    term: lease.term,
                    ttl,
                    deadline: now + ttl,
                });
                ElectionEvent::None
            }
        }
    }

    /// Apply a received `CoordinatorClaim { candidate, term }` (spec §7.10).
    ///
    /// A `CoordinatorClaim` is a **campaign announcement, not a lease**: it advertises
    /// that `candidate` *intends* to lead at `term`, but it carries no TTL and grants no
    /// liveness. This machine therefore **never installs `candidate` as the believed
    /// coordinator from a claim** (that would fabricate a coordinator with a synthetic
    /// deadline and let an unauthenticated/replayed claim invent leadership). The
    /// believed coordinator changes only when a real [`Lease`] arrives (see
    /// [`Election::on_lease`]). A claim only ever:
    /// - advances `seen_term` (so our own future campaign out-terms it), and
    /// - if **we** are the current holder, decides whether to abdicate or defend:
    ///   - claim **strictly superior** to us → we drop our lease and emit
    ///     [`ElectionEvent::Yield`] (we abdicate; the candidate must still earn the
    ///     lease via its own `CoordinatorLease`),
    ///   - claim **inferior** to us → we re-assert via [`ElectionEvent::RenewLease`] so
    ///     the candidate backs off.
    ///
    /// When we are *not* the holder, a claim is purely informational (it bumps
    /// `seen_term`); it neither installs nor evicts whatever lease we currently believe.
    pub fn on_claim(&mut self, candidate: DeviceId, term: u64, _now: Instant) -> ElectionEvent {
        self.seen_term = self.seen_term.max(term);

        let Some(current) = self.held else {
            // No believed coordinator. A bare claim is not a lease, so we do NOT
            // fabricate one here — just observe the term and wait for a real lease.
            return ElectionEvent::None;
        };

        if current.holder != self.me {
            // The claim targets / contends with a holder that isn't us. We don't
            // install the candidate (no lease yet) and we don't evict the holder we
            // believe in on the strength of an unauthenticated claim — its lease still
            // expires via `tick` if it is truly gone. Informational only.
            return ElectionEvent::None;
        }

        // We are the current holder: decide whether to abdicate or defend.
        if Self::supersedes(term, candidate, current.term, current.holder) {
            // The candidate out-ranks us. Abdicate by dropping our lease (we leave no
            // coordinator believed; the candidate still has to win the lease itself).
            self.held = None;
            ElectionEvent::Yield {
                from: self.me,
                term,
            }
        } else {
            // Our lease is superior — re-assert it so the candidate backs off.
            ElectionEvent::RenewLease(self.current_lease(current))
        }
    }

    /// Apply a received `CoordinatorYield { from, term }` (spec §7.10).
    ///
    /// The lease is dropped **only** when the yield is from the current holder *at the
    /// exact same term* (`current.term == term`). A yield carrying any other term —
    /// higher (stale/forged/reordered) or lower (replayed) — must NOT knock out a valid
    /// coordinator: a `from` device only yields the term it actually holds, so a
    /// non-matching term is not a real abdication of the lease we believe in.
    ///
    /// The yield's term is also folded into `seen_term` so a subsequent *legitimate*
    /// claim can't reuse a now-stale term (otherwise a campaign after a high-term yield
    /// could collide with the term that yield observed).
    pub fn on_yield(&mut self, from: DeviceId, term: u64) -> ElectionEvent {
        self.seen_term = self.seen_term.max(term);
        if let Some(current) = self.held {
            if current.holder == from && current.term == term {
                self.held = None;
            }
        }
        ElectionEvent::None
    }

    /// Advance time. If the current lease has expired (`now >= deadline`) it is
    /// dropped, leaving no coordinator. If this device is the holder and at least
    /// `ttl/3` has **elapsed** since the lease was started/last renewed, returns
    /// [`ElectionEvent::RenewLease`] so the engine re-advertises — i.e. the holder
    /// re-announces roughly 3 times per TTL (spec §7.10 "renew at `ttl/3`"). This is
    /// time *elapsed since the last renewal*, NOT `ttl/3` remaining before expiry.
    pub fn tick(&mut self, now: Instant) -> ElectionEvent {
        let Some(current) = self.held else {
            return ElectionEvent::None;
        };

        if now >= current.deadline {
            self.held = None;
            return ElectionEvent::None;
        }

        if current.holder == self.me {
            // deadline = last_start + ttl, so last_start = deadline - ttl and the
            // renewal point is last_start + ttl/3 = deadline - ttl + ttl/3.
            let renew_at = current.deadline - current.ttl + (current.ttl / 3);
            if now >= renew_at {
                let renewed = Held {
                    deadline: now + current.ttl,
                    ..current
                };
                self.held = Some(renewed);
                return ElectionEvent::RenewLease(self.current_lease(renewed));
            }
        }
        ElectionEvent::None
    }

    /// Start a campaign for coordinator: increment the term beyond the highest seen and
    /// emit an [`ElectionEvent::Claim`] (spec §7.10 "candidate increments term").
    ///
    /// **Guarded — does NOT unconditionally usurp.** A higher term always *wins* the
    /// term comparison, so campaigning whenever we feel like it would let any node
    /// preempt a perfectly healthy coordinator at will (an unauthenticated-liveness
    /// hazard). We therefore campaign only when it is legitimate to do so:
    /// - there is **no believed coordinator** (none yet, or the last lease has expired
    ///   by `now`), **or**
    /// - **we are already the holder** (re-campaign to defend / bump our own term), **or**
    /// - the held lease is **provably inferior to us**: at equal term the sole
    ///   deterministic tiebreak is the lowest `device_id`, so if our id is lower than
    ///   the current holder's we are the rightful winner and may challenge it.
    ///
    /// Otherwise (a valid coordinator with a lower-or-equal — i.e. winning — id holds
    /// the lease) we back off and return [`ElectionEvent::None`], leaving `seen_term`
    /// untouched so we don't needlessly inflate the term space.
    ///
    /// On a legitimate campaign we provisionally take the lease locally at the new term
    /// (peers confirm via their own term rules) and emit the `Claim`.
    pub fn start_claim(&mut self, now: Instant) -> ElectionEvent {
        if !self.may_campaign(now) {
            return ElectionEvent::None;
        }
        let term = self.seen_term.saturating_add(1);
        self.seen_term = term;
        // Provisionally take the lease locally at the new term; peers confirm via
        // their own term rules. Use the current lease's TTL or the default.
        let ttl = self.held.map_or(DEFAULT_TTL, |h| h.ttl);
        self.held = Some(Held {
            holder: self.me,
            term,
            ttl,
            deadline: now + ttl,
        });
        ElectionEvent::Claim {
            candidate: self.me,
            term,
        }
    }

    /// Whether a [`Election::start_claim`] campaign is legitimate right now (see that
    /// method's docs): no valid coordinator, we already hold it, or the held lease is
    /// provably inferior to us by the lowest-`device_id` tiebreak.
    fn may_campaign(&self, now: Instant) -> bool {
        match self.held {
            None => true,
            Some(current) => {
                // An expired lease is not a valid coordinator.
                if now >= current.deadline {
                    return true;
                }
                // We already hold it, or we win the deterministic id tiebreak.
                current.holder == self.me || self.me < current.holder
            }
        }
    }

    fn current_lease(&self, held: Held) -> Lease {
        Lease {
            holder: held.holder,
            term: held.term,
            ttl: held.ttl,
        }
    }
}
