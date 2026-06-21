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
    /// If this device is the current holder and the claim is **strictly superior**
    /// (higher term, or equal term with a lower candidate id), it yields and the
    /// candidate becomes the believed coordinator. Otherwise the claim is ignored
    /// (the rightful holder keeps/asserts its lease).
    pub fn on_claim(&mut self, candidate: DeviceId, term: u64, now: Instant) -> ElectionEvent {
        self.seen_term = self.seen_term.max(term);

        match self.held {
            Some(current) => {
                if Self::supersedes(term, candidate, current.term, current.holder) {
                    let was_me = current.holder == self.me;
                    self.held = Some(Held {
                        holder: candidate,
                        term,
                        ttl: current.ttl,
                        deadline: now + current.ttl,
                    });
                    if was_me {
                        return ElectionEvent::Yield {
                            from: self.me,
                            term,
                        };
                    }
                    ElectionEvent::None
                } else {
                    // Our (or the current holder's) lease is superior. If it is ours,
                    // re-assert it so the candidate backs off.
                    if current.holder == self.me {
                        return ElectionEvent::RenewLease(self.current_lease(current));
                    }
                    ElectionEvent::None
                }
            }
            None => {
                self.held = Some(Held {
                    holder: candidate,
                    term,
                    ttl: DEFAULT_TTL,
                    deadline: now + DEFAULT_TTL,
                });
                ElectionEvent::None
            }
        }
    }

    /// Apply a received `CoordinatorYield { from, term }` (spec §7.10). If the yielding
    /// device is the current holder at this term, the lease is dropped, leaving no
    /// coordinator until a new claim/lease arrives.
    pub fn on_yield(&mut self, from: DeviceId, term: u64) -> ElectionEvent {
        if let Some(current) = self.held {
            if current.holder == from && current.term <= term {
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

    /// Start a campaign for coordinator: increment the term beyond the highest seen
    /// and emit a [`ElectionEvent::Claim`] (spec §7.10 "candidate increments term").
    /// Use when there is no coordinator, or to challenge an inferior one.
    pub fn start_claim(&mut self, now: Instant) -> ElectionEvent {
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

    fn current_lease(&self, held: Held) -> Lease {
        Lease {
            holder: held.holder,
            term: held.term,
            ttl: held.ttl,
        }
    }
}
