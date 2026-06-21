//! Integration tests for the lease-based coordinator election state machine
//! (spec §7.10). These live outside `src/election.rs` to keep that module under
//! the 500-line source budget while preserving full coverage. They exercise the
//! public API only.

use std::time::{Duration, Instant};

use mouser_core::election::{DEFAULT_TTL, MAX_TTL};
use mouser_core::{DeviceId, Election, ElectionEvent, Lease};

const A: DeviceId = [0xAA; 32];
const B: DeviceId = [0xBB; 32];
const LOW: DeviceId = [0x01; 32];

fn lease(holder: DeviceId, term: u64) -> Lease {
    Lease {
        holder,
        term,
        ttl: DEFAULT_TTL,
    }
}

#[test]
fn accepts_first_lease_and_sets_deadline() {
    let t0 = Instant::now();
    let mut e = Election::new(A);
    assert_eq!(e.on_lease(lease(B, 1), t0), ElectionEvent::None);
    assert_eq!(e.coordinator(), Some(B));
    assert_eq!(e.term(), Some(1));
    assert_eq!(e.deadline(), Some(t0 + DEFAULT_TTL));
}

#[test]
fn lease_expires_after_ttl() {
    let t0 = Instant::now();
    let mut e = Election::new(A);
    e.on_lease(lease(B, 1), t0);
    // Just before expiry: still held.
    let _ = e.tick(t0 + DEFAULT_TTL - Duration::from_millis(1));
    assert_eq!(e.coordinator(), Some(B));
    // At/after deadline: expired.
    let _ = e.tick(t0 + DEFAULT_TTL);
    assert_eq!(e.coordinator(), None);
}

#[test]
fn holder_renews_at_ttl_over_three() {
    let t0 = Instant::now();
    let mut e = Election::new(A);
    // A becomes holder via its own claim.
    assert_eq!(
        e.start_claim(t0),
        ElectionEvent::Claim {
            candidate: A,
            term: 1
        }
    );
    assert!(e.is_coordinator());
    // Before renewal point: no renew.
    let before = t0 + DEFAULT_TTL - DEFAULT_TTL / 3 - Duration::from_millis(1);
    assert_eq!(e.tick(before), ElectionEvent::None);
    // At renewal point (ttl/3 before expiry): renew.
    let at = t0 + DEFAULT_TTL - DEFAULT_TTL / 3;
    match e.tick(at) {
        ElectionEvent::RenewLease(l) => {
            assert_eq!(l.holder, A);
            assert_eq!(l.term, 1);
        }
        other => panic!("expected RenewLease, got {other:?}"),
    }
    // Deadline pushed forward by the renew.
    assert_eq!(e.deadline(), Some(at + DEFAULT_TTL));
}

#[test]
fn non_holder_never_renews() {
    let t0 = Instant::now();
    let mut e = Election::new(A);
    e.on_lease(lease(B, 1), t0);
    // Even at the renewal point, A is not the holder -> no renew.
    let at = t0 + DEFAULT_TTL - DEFAULT_TTL / 3;
    assert_eq!(e.tick(at), ElectionEvent::None);
}

#[test]
fn higher_term_lease_supersedes_and_yields() {
    let t0 = Instant::now();
    let mut e = Election::new(A);
    // A is holder at term 1.
    e.start_claim(t0);
    assert!(e.is_coordinator());
    // B advertises a strictly-higher term -> A yields.
    match e.on_lease(lease(B, 2), t0) {
        ElectionEvent::Yield { from, term } => {
            assert_eq!(from, A);
            assert_eq!(term, 2);
        }
        other => panic!("expected Yield, got {other:?}"),
    }
    assert_eq!(e.coordinator(), Some(B));
    assert_eq!(e.term(), Some(2));
}

#[test]
fn equal_term_tiebreak_lower_device_id_wins() {
    let t0 = Instant::now();
    // Observer C sees two holders at the same term; the lower id must win.
    let mut e = Election::new([0xCC; 32]);
    e.on_lease(lease(A, 5), t0);
    assert_eq!(e.coordinator(), Some(A));
    // LOW < A at the same term -> LOW supersedes.
    assert_eq!(e.on_lease(lease(LOW, 5), t0), ElectionEvent::None);
    assert_eq!(e.coordinator(), Some(LOW));
    // A re-advertises at the same term -> does NOT supersede LOW.
    assert_eq!(e.on_lease(lease(A, 5), t0), ElectionEvent::None);
    assert_eq!(e.coordinator(), Some(LOW), "lower id retains at equal term");
}

#[test]
fn equal_term_tiebreak_when_we_are_the_loser_yields() {
    let t0 = Instant::now();
    // A is holder at term 5; LOW (lower id) advertises the same term -> A yields.
    let mut e = Election::new(A);
    e.on_lease(lease(A, 5), t0); // adopt our own as current
    assert_eq!(e.coordinator(), Some(A));
    match e.on_lease(lease(LOW, 5), t0) {
        ElectionEvent::Yield { from, term } => {
            assert_eq!(from, A);
            assert_eq!(term, 5);
        }
        other => panic!("expected Yield, got {other:?}"),
    }
    assert_eq!(e.coordinator(), Some(LOW));
}

#[test]
fn partition_heal_two_holders_meet_higher_term_wins() {
    let t0 = Instant::now();
    // Partition: node X believes it is coordinator at term 3; node Y at term 4.
    // When they meet, Y's higher term wins on both sides.
    // From X's perspective (X = A, holder of term 3):
    let mut x = Election::new(A);
    x.start_claim(t0); // term 1
    x.start_claim(t0); // term 2
    x.start_claim(t0); // term 3
    assert_eq!(x.term(), Some(3));
    assert!(x.is_coordinator());
    // X hears Y's (B) lease at term 4 -> X yields, Y wins.
    match x.on_lease(lease(B, 4), t0) {
        ElectionEvent::Yield { from, term } => {
            assert_eq!(from, A);
            assert_eq!(term, 4);
        }
        other => panic!("expected Yield, got {other:?}"),
    }
    assert_eq!(x.coordinator(), Some(B));
    assert_eq!(x.term(), Some(4));

    // From Y's perspective (Y = B, holder of term 4): hears X's stale term-3
    // lease -> ignores it, stays coordinator.
    let mut y = Election::new(B);
    y.start_claim(t0);
    y.start_claim(t0);
    y.start_claim(t0);
    y.start_claim(t0); // term 4
    assert_eq!(y.term(), Some(4));
    assert_eq!(y.on_lease(lease(A, 3), t0), ElectionEvent::None);
    assert_eq!(y.coordinator(), Some(B));
    assert_eq!(y.term(), Some(4));
}

#[test]
fn claim_increments_beyond_seen_term() {
    let t0 = Instant::now();
    let mut e = Election::new(A);
    // Observe a term-7 lease, then campaign: claim must be term 8.
    e.on_lease(lease(B, 7), t0);
    match e.start_claim(t0) {
        ElectionEvent::Claim { candidate, term } => {
            assert_eq!(candidate, A);
            assert_eq!(term, 8);
        }
        other => panic!("expected Claim term 8, got {other:?}"),
    }
}

#[test]
fn holder_reasserts_lease_against_inferior_claim() {
    let t0 = Instant::now();
    let mut e = Election::new(LOW);
    e.start_claim(t0); // LOW holds term 1
    assert!(e.is_coordinator());
    // A (higher id) claims the SAME term 1 -> LOW is superior, re-asserts.
    match e.on_claim(A, 1, t0) {
        ElectionEvent::RenewLease(l) => {
            assert_eq!(l.holder, LOW);
            assert_eq!(l.term, 1);
        }
        other => panic!("expected RenewLease, got {other:?}"),
    }
    assert_eq!(e.coordinator(), Some(LOW));
}

#[test]
fn holder_yields_to_superior_claim() {
    let t0 = Instant::now();
    let mut e = Election::new(A);
    e.start_claim(t0); // A holds term 1
                       // A higher-term claim from B -> A yields, B becomes coordinator.
    match e.on_claim(B, 2, t0) {
        ElectionEvent::Yield { from, term } => {
            assert_eq!(from, A);
            assert_eq!(term, 2);
        }
        other => panic!("expected Yield, got {other:?}"),
    }
    assert_eq!(e.coordinator(), Some(B));
    assert_eq!(e.term(), Some(2));
}

#[test]
fn yield_from_holder_drops_lease() {
    let t0 = Instant::now();
    let mut e = Election::new(A);
    e.on_lease(lease(B, 3), t0);
    assert_eq!(e.coordinator(), Some(B));
    assert_eq!(e.on_yield(B, 3), ElectionEvent::None);
    assert_eq!(e.coordinator(), None);
}

#[test]
fn yield_from_non_holder_is_ignored() {
    let t0 = Instant::now();
    let mut e = Election::new(A);
    e.on_lease(lease(B, 3), t0);
    // A yield from someone who isn't the current holder changes nothing.
    assert_eq!(e.on_yield([0x77; 32], 3), ElectionEvent::None);
    assert_eq!(e.coordinator(), Some(B));
}

#[test]
fn ttl_is_clamped_to_max() {
    let t0 = Instant::now();
    let mut e = Election::new(A);
    let huge = Lease {
        holder: B,
        term: 1,
        ttl: Duration::from_secs(120),
    };
    e.on_lease(huge, t0);
    assert_eq!(e.deadline(), Some(t0 + MAX_TTL));
}

#[test]
fn renewal_from_same_holder_refreshes_deadline() {
    let t0 = Instant::now();
    let mut e = Election::new(A);
    e.on_lease(lease(B, 1), t0);
    let later = t0 + Duration::from_millis(1000);
    e.on_lease(lease(B, 1), later);
    assert_eq!(e.deadline(), Some(later + DEFAULT_TTL));
}
