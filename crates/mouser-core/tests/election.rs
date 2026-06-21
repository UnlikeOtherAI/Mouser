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
    // Renewal fires after ttl/3 has ELAPSED since the lease start (re-announce ~3x
    // per ttl), NOT when ttl/3 remains before expiry.
    // Just before ttl/3 elapsed: no renew.
    let before = t0 + DEFAULT_TTL / 3 - Duration::from_millis(1);
    assert_eq!(e.tick(before), ElectionEvent::None);
    // Sanity: at the OLD (wrong) "ttl/3 before expiry" point we'd have renewed; here
    // it must already have renewed well before that, so no assertion at 2/3 needed.
    // At ttl/3 elapsed: renew.
    let at = t0 + DEFAULT_TTL / 3;
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
fn renew_fires_at_ttl_over_three_elapsed_not_remaining() {
    // Regression guard for the renew predicate: it must use ttl/3 ELAPSED, not
    // ttl/3 remaining (= 2*ttl/3 elapsed). Tick at just past ttl/3 elapsed and the
    // FIRST renew must already fire — under the old "remaining" formula it would not.
    let t0 = Instant::now();
    let mut e = Election::new(A);
    e.start_claim(t0);
    let just_after_third = t0 + DEFAULT_TTL / 3 + Duration::from_millis(1);
    match e.tick(just_after_third) {
        ElectionEvent::RenewLease(l) => assert_eq!(l.holder, A),
        other => panic!("expected RenewLease at ~ttl/3 elapsed, got {other:?}"),
    }
}

#[test]
fn renews_about_three_times_per_ttl() {
    // Re-announce cadence: stepping the clock by ttl/3 each time yields a renew on
    // every step (~3 renewals per ttl), confirming elapsed-since-renewal semantics.
    let t0 = Instant::now();
    let mut e = Election::new(A);
    e.start_claim(t0);
    let step = DEFAULT_TTL / 3;
    for i in 1..=3u32 {
        let now = t0 + step * i;
        match e.tick(now) {
            ElectionEvent::RenewLease(l) => assert_eq!(l.holder, A),
            other => panic!("expected RenewLease on step {i}, got {other:?}"),
        }
        assert_eq!(e.deadline(), Some(now + DEFAULT_TTL));
    }
}

#[test]
fn non_holder_never_renews() {
    let t0 = Instant::now();
    let mut e = Election::new(A);
    e.on_lease(lease(B, 1), t0);
    // Even at the renewal point (ttl/3 elapsed), A is not the holder -> no renew.
    let at = t0 + DEFAULT_TTL / 3;
    assert_eq!(e.tick(at), ElectionEvent::None);
}

#[test]
fn higher_term_lease_supersedes_and_yields() {
    let t0 = Instant::now();
    let mut e = Election::new(A);
    // A is holder at term 1.
    e.start_claim(t0);
    assert!(e.is_coordinator());
    // B advertises a strictly-higher term -> A yields. The Yield carries A's
    // RELINQUISHED term (1, the lease A is giving up), not B's superior term (2),
    // so a peer's exact-match `on_yield` drops the right (A@1) stale lease.
    match e.on_lease(lease(B, 2), t0) {
        ElectionEvent::Yield { from, term } => {
            assert_eq!(from, A);
            assert_eq!(term, 1, "yield carries A's relinquished term, not B's");
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
    // Same-term tiebreak: A's relinquished term (5) equals the incoming term, so the
    // Yield carries 5 either way.
    let mut e = Election::new(A);
    e.on_lease(lease(A, 5), t0); // adopt our own as current
    assert_eq!(e.coordinator(), Some(A));
    match e.on_lease(lease(LOW, 5), t0) {
        ElectionEvent::Yield { from, term } => {
            assert_eq!(from, A);
            assert_eq!(
                term, 5,
                "relinquished term == incoming term at equal-term tiebreak"
            );
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
    // X hears Y's (B) lease at term 4 -> X yields, Y wins. X's Yield carries its own
    // RELINQUISHED term (3), not Y's superior term (4).
    match x.on_lease(lease(B, 4), t0) {
        ElectionEvent::Yield { from, term } => {
            assert_eq!(from, A);
            assert_eq!(term, 3, "yield carries X's relinquished term, not Y's");
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
fn holder_yields_to_superior_claim_but_does_not_install_candidate() {
    let t0 = Instant::now();
    let mut e = Election::new(A);
    e.start_claim(t0); // A holds term 1
                       // A higher-term claim from B -> A abdicates (yields), but a bare
                       // CoordinatorClaim is NOT a lease, so B is NOT installed as the
                       // believed coordinator: we drop to "no coordinator" until B's real
                       // CoordinatorLease arrives.
    match e.on_claim(B, 2, t0) {
        ElectionEvent::Yield { from, term } => {
            assert_eq!(from, A);
            assert_eq!(
                term, 1,
                "yield carries A's relinquished term (1), not the claim's term (2)"
            );
        }
        other => panic!("expected Yield, got {other:?}"),
    }
    assert_eq!(
        e.coordinator(),
        None,
        "bare claim must not install a coordinator"
    );
    assert_eq!(e.term(), None);
    // The claim's term WAS observed, so B's later real lease at term 2 is accepted and
    // A's stale term-1 re-advert can't resurrect A.
    e.on_lease(lease(B, 2), t0);
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

// ---------------------------------------------------------------------------
// Round-2 audit regressions: yield / claim / term edge cases (untested before).
// ---------------------------------------------------------------------------

#[test]
fn yield_with_term_above_holder_does_not_drop() {
    // R2: on_yield must require the EXACT holder term. A yield carrying a term
    // *higher* than the lease we believe in (stale/forged/reordered) must NOT knock
    // out the valid coordinator — a holder only yields the term it actually holds.
    let t0 = Instant::now();
    let mut e = Election::new(A);
    e.on_lease(lease(B, 3), t0);
    assert_eq!(e.coordinator(), Some(B));
    assert_eq!(e.on_yield(B, 4), ElectionEvent::None);
    assert_eq!(
        e.coordinator(),
        Some(B),
        "term>holder yield must not drop the lease"
    );
    assert_eq!(e.term(), Some(3));
}

#[test]
fn yield_with_term_below_holder_does_not_drop() {
    // R2 (mirror): a replayed yield carrying a term *below* the current lease must
    // also be ignored — only an exact-term yield from the holder abdicates.
    let t0 = Instant::now();
    let mut e = Election::new(A);
    e.on_lease(lease(B, 5), t0);
    assert_eq!(e.on_yield(B, 4), ElectionEvent::None);
    assert_eq!(
        e.coordinator(),
        Some(B),
        "term<holder yield must not drop the lease"
    );
}

#[test]
fn duplicate_same_term_yield_after_renew_does_not_drop() {
    // R2: a legit yield drops the lease; a DUPLICATE of that same yield, replayed
    // after the (same) holder has renewed/re-advertised, must NOT drop the freshly
    // re-established lease. The exact-term check still matches, so the new defence is
    // that the holder re-acquired via on_lease and the stale duplicate cannot re-drop
    // a coordinator it never observed leaving.
    let t0 = Instant::now();
    let mut e = Election::new(A);
    e.on_lease(lease(B, 3), t0);
    // Holder B genuinely yields at term 3 -> dropped.
    assert_eq!(e.on_yield(B, 3), ElectionEvent::None);
    assert_eq!(e.coordinator(), None);
    // B comes back with a fresh lease at a HIGHER term (a new campaign won the lease).
    e.on_lease(lease(B, 4), t0 + Duration::from_millis(10));
    assert_eq!(e.coordinator(), Some(B));
    assert_eq!(e.term(), Some(4));
    // The OLD term-3 yield is replayed/duplicated: it does not match the live term 4
    // and must NOT drop the renewed lease.
    assert_eq!(e.on_yield(B, 3), ElectionEvent::None);
    assert_eq!(
        e.coordinator(),
        Some(B),
        "stale duplicate yield must not re-drop a renewed lease"
    );
    assert_eq!(e.term(), Some(4));
}

#[test]
fn yield_term_is_observed_into_seen_term() {
    // R2: a yield's term must count toward seen_term so a subsequent legitimate
    // campaign can't reuse a now-stale term. Observe a high-term yield (with no
    // holder, so it only updates seen_term), then campaign: the claim must out-term it.
    let t0 = Instant::now();
    let mut e = Election::new(A);
    assert_eq!(e.on_yield(B, 9), ElectionEvent::None);
    assert_eq!(e.coordinator(), None);
    match e.start_claim(t0) {
        ElectionEvent::Claim { candidate, term } => {
            assert_eq!(candidate, A);
            assert_eq!(
                term, 10,
                "claim must increment beyond the observed yield term"
            );
        }
        other => panic!("expected Claim term 10, got {other:?}"),
    }
}

#[test]
fn on_claim_on_fresh_election_does_not_assert_a_coordinator() {
    // R2: a bare CoordinatorClaim is NOT a lease. On a fresh Election it must only
    // observe the term — never fabricate a believed coordinator with a synthetic
    // DEFAULT_TTL deadline.
    let t0 = Instant::now();
    let mut e = Election::new(A);
    assert_eq!(e.on_claim(B, 4, t0), ElectionEvent::None);
    assert_eq!(
        e.coordinator(),
        None,
        "a bare claim must not install a coordinator"
    );
    assert_eq!(e.term(), None);
    assert_eq!(
        e.deadline(),
        None,
        "no synthetic full-TTL deadline from a claim"
    );
    // The term WAS observed: our own later campaign out-terms it.
    match e.start_claim(t0) {
        ElectionEvent::Claim { term, .. } => assert_eq!(term, 5),
        other => panic!("expected Claim term 5, got {other:?}"),
    }
}

#[test]
fn claim_against_other_holder_does_not_evict_or_install() {
    // R2: when we are NOT the holder, a claim is informational only — it neither
    // installs the candidate nor evicts the holder we currently believe in (its lease
    // still expires via tick if truly gone).
    let t0 = Instant::now();
    let mut e = Election::new([0xCC; 32]);
    e.on_lease(lease(B, 2), t0); // we believe B holds
    assert_eq!(e.on_claim(A, 3, t0), ElectionEvent::None);
    assert_eq!(
        e.coordinator(),
        Some(B),
        "claim must not evict the believed holder"
    );
    assert_eq!(e.term(), Some(2));
}

#[test]
fn replayed_stale_lease_does_not_extend_or_resurrect() {
    // R2: a lease whose term is BELOW an already-seen term is stale/replayed and must
    // not resurrect a coordinator after the cluster moved on. Move seen_term to 5 via
    // a higher-term lease, let it expire, then replay an old term-2 lease.
    let t0 = Instant::now();
    let mut e = Election::new(A);
    e.on_lease(lease(B, 5), t0);
    assert_eq!(e.term(), Some(5));
    // Expire the lease.
    let _ = e.tick(t0 + DEFAULT_TTL);
    assert_eq!(e.coordinator(), None);
    // Replay a stale term-2 lease from a different holder: must NOT resurrect.
    assert_eq!(
        e.on_lease(lease([0x22; 32], 2), t0 + DEFAULT_TTL),
        ElectionEvent::None
    );
    assert_eq!(
        e.coordinator(),
        None,
        "stale lease below seen_term must not resurrect"
    );
}

#[test]
fn stale_lease_does_not_extend_a_live_superior_lease() {
    // R2 (mirror, holder-present path): with a live higher-term holder, an inferior
    // replayed lease from another holder must be ignored and must not push the
    // deadline of the live lease.
    let t0 = Instant::now();
    let mut e = Election::new(A);
    e.on_lease(lease(B, 5), t0);
    let d0 = e.deadline();
    let later = t0 + Duration::from_millis(500);
    assert_eq!(e.on_lease(lease([0x22; 32], 2), later), ElectionEvent::None);
    assert_eq!(e.coordinator(), Some(B));
    assert_eq!(
        e.deadline(),
        d0,
        "an inferior replayed lease must not extend the live lease"
    );
}

#[test]
fn start_claim_does_not_usurp_a_valid_lower_id_coordinator() {
    // R2: start_claim must NOT unconditionally usurp. A valid coordinator with a
    // LOWER (winning) device_id holds the lease -> we must back off and not inflate
    // seen_term.
    let t0 = Instant::now();
    let mut e = Election::new(A); // A = 0xAA
    e.on_lease(lease(LOW, 2), t0); // LOW < A, valid lease
    assert_eq!(
        e.start_claim(t0),
        ElectionEvent::None,
        "must not usurp a winning coordinator"
    );
    assert_eq!(e.coordinator(), Some(LOW), "valid coordinator retained");
    assert_eq!(
        e.term(),
        Some(2),
        "seen_term/term not inflated by a refused campaign"
    );
}

#[test]
fn start_claim_challenges_an_inferior_higher_id_coordinator() {
    // R2: the held lease is provably inferior to us (holder has a HIGHER id), so at
    // equal term we'd win the tiebreak -> a campaign is legitimate.
    let t0 = Instant::now();
    let mut e = Election::new(LOW); // LOW
    e.on_lease(lease(A, 2), t0); // A > LOW
    match e.start_claim(t0) {
        ElectionEvent::Claim { candidate, term } => {
            assert_eq!(candidate, LOW);
            assert_eq!(term, 3, "campaign increments beyond the seen term");
        }
        other => panic!("expected Claim, got {other:?}"),
    }
    assert!(e.is_coordinator());
}

#[test]
fn start_claim_campaigns_when_lease_has_expired() {
    // R2: an expired lease is not a valid coordinator even if its holder won the id
    // tiebreak -> we may campaign.
    let t0 = Instant::now();
    let mut e = Election::new(A);
    e.on_lease(lease(LOW, 2), t0); // LOW < A but...
    let after_expiry = t0 + DEFAULT_TTL; // ...lease has expired.
    match e.start_claim(after_expiry) {
        ElectionEvent::Claim { candidate, term } => {
            assert_eq!(candidate, A);
            assert_eq!(term, 3);
        }
        other => panic!("expected Claim after expiry, got {other:?}"),
    }
}

#[test]
fn start_claim_redefends_when_we_already_hold() {
    // R2: re-campaigning while we are the holder is allowed (bump our own term).
    let t0 = Instant::now();
    let mut e = Election::new(A);
    e.start_claim(t0); // term 1, A holds
    match e.start_claim(t0) {
        ElectionEvent::Claim { candidate, term } => {
            assert_eq!(candidate, A);
            assert_eq!(term, 2);
        }
        other => panic!("expected Claim term 2, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Gate regression: cross-node abdication Yield must carry the RELINQUISHED lease
// term so a peer's exact-match `on_yield` drops the right stale lease. Before the
// fix the Yield carried the SUPERIOR incoming term (T+1), the peer's `== term`
// check failed, and the cross-node yield was a no-op (stale lease never dropped).
// ---------------------------------------------------------------------------

#[test]
fn cross_node_yield_from_on_claim_abdication_drops_peers_stale_lease() {
    let t0 = Instant::now();
    // H holds a lease at term T (=2).
    let mut h = Election::new(B);
    h.on_lease(lease(B, 2), t0);
    assert_eq!(h.coordinator(), Some(B));

    // A superior candidate C claims at T+1 -> H abdicates and emits a Yield. The
    // captured Yield must carry H's RELINQUISHED term T (2), NOT the claim's T+1 (3).
    let (from, term) = match h.on_claim(LOW, 3, t0) {
        ElectionEvent::Yield { from, term } => (from, term),
        other => panic!("expected Yield from abdication, got {other:?}"),
    };
    assert_eq!(from, B);
    assert_eq!(
        term, 2,
        "abdication yield must carry the relinquished term T, not T+1"
    );

    // Peer X independently believes H@T (=2). Feeding it the captured Yield must drop
    // the stale lease. Pre-fix the term would have been 3 and `on_yield`'s exact-match
    // (current.term == term) would FAIL, leaving X.coordinator() == Some(B).
    let mut x = Election::new(A);
    x.on_lease(lease(B, 2), t0);
    assert_eq!(x.coordinator(), Some(B));
    assert_eq!(x.on_yield(from, term), ElectionEvent::None);
    assert_eq!(
        x.coordinator(),
        None,
        "peer must drop the stale lease on the relinquished-term yield"
    );
}

#[test]
fn cross_node_yield_from_on_lease_supersede_drops_peers_stale_lease() {
    let t0 = Instant::now();
    // H holds a lease at term T (=2).
    let mut h = Election::new(B);
    h.on_lease(lease(B, 2), t0);
    assert_eq!(h.coordinator(), Some(B));

    // A superior lease from C at T+1 supersedes H -> H emits a Yield. The captured
    // Yield must carry H's RELINQUISHED term T (2), NOT the superseding lease's T+1.
    let (from, term) = match h.on_lease(lease(LOW, 3), t0) {
        ElectionEvent::Yield { from, term } => (from, term),
        other => panic!("expected Yield from supersede, got {other:?}"),
    };
    assert_eq!(from, B);
    assert_eq!(
        term, 2,
        "supersede yield must carry the relinquished term T, not the superior lease's term"
    );
    // H itself now believes the superior holder at T+1.
    assert_eq!(h.coordinator(), Some(LOW));
    assert_eq!(h.term(), Some(3));

    // Peer X independently believes H@T (=2). The captured Yield must drop it. Pre-fix
    // term would be 3 and the exact-match in `on_yield` would FAIL, leaving Some(B).
    let mut x = Election::new(A);
    x.on_lease(lease(B, 2), t0);
    assert_eq!(x.coordinator(), Some(B));
    assert_eq!(x.on_yield(from, term), ElectionEvent::None);
    assert_eq!(
        x.coordinator(),
        None,
        "peer must drop the stale lease on the relinquished-term yield"
    );
}
