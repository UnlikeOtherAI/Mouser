//! Microbenchmarks for the target-side `EngineCore` dispatch hot path: the per-event
//! work done on every received motion datagram and key frame — anti-replay, transition
//! classification, rate-cap, and action construction (plus CBOR decode for keys). The
//! actual OS injection (`SendInput`/`GetSystemMetrics`/`EnumDisplayMonitors`) is NOT
//! benchmarked here: those are real syscalls that move the cursor and whose timing
//! reflects the OS/driver, not engine code (and must not run unattended).
//!
//! Dependency-free, prints ns/op. Run:
//!   cargo test -p mouser-engine --test perf_core -- --nocapture

use std::hint::black_box;
use std::time::Instant;

use mouser_engine::core::EngineCore;
use mouser_protocol::{
    to_cbor, Datagram, KeyEvent, OwnershipTransfer, PointerMotion, TransferReason, TYPE_KEY_EVENT,
    TYPE_OWNERSHIP_TRANSFER,
};

const ME: [u8; 32] = [1u8; 32];
const PEER: [u8; 32] = [2u8; 32];

/// A target engine that has already accepted an ownership grant at epoch 1, so injection
/// is authorized and the dispatch stays on the inject path.
fn granted_target() -> EngineCore {
    let mut t = EngineCore::new_target(ME, PEER);
    let grant = to_cbor(&OwnershipTransfer {
        to: ME.to_vec(),
        owner_epoch: 1,
        layout_rev: 0,
        reason: TransferReason::EdgeCross,
    })
    .unwrap_or_default();
    t.on_control(TYPE_OWNERSHIP_TRANSFER, &grant);
    t
}

#[test]
fn perf_core_target_dispatch_report() {
    // ---- MOTION: on_motion takes an already-decoded Datagram (decode is benched in
    //      mouser-protocol). Motion is exempt from the rate bucket, so strictly-increasing
    //      seq keeps every call on the inject path. ----
    let motion_ns = {
        const ITERS: u32 = 1_000_000;
        let mut t = granted_target();
        for seq in 1..=50_000u32 {
            black_box(t.on_motion(black_box(Datagram::Motion(PointerMotion {
                owner_epoch: 1,
                seq,
                display_id: 0,
                x: 1920,
                y: 1080,
            }))));
        }
        let t0 = Instant::now();
        for seq in 50_001..=(50_000 + ITERS) {
            black_box(t.on_motion(black_box(Datagram::Motion(PointerMotion {
                owner_epoch: 1,
                seq,
                display_id: 0,
                x: 1920,
                y: 1080,
            }))));
        }
        t0.elapsed().as_nanos() as f64 / f64::from(ITERS)
    };

    // ---- KEY: on_control(TYPE_KEY_EVENT, cbor) — CBOR decode + anti-replay + transition
    //      classification + rate-cap + action build. Pre-generate distinct-usage
    //      press/release pairs with strictly-increasing ctr so every event is a transition
    //      (always injects) and never a replay; this isolates the dispatch cost (payload
    //      bytes are built outside the timed loop). ----
    let key_ns = {
        const ITERS: u32 = 300_000;
        const USAGES: u64 = 1_000;
        let payloads: Vec<Vec<u8>> = (0..u64::from(ITERS))
            .map(|i| {
                let usage = 0x04 + ((i / 2) % USAGES) as u16; // press then release per usage
                let down = i % 2 == 0;
                to_cbor(&KeyEvent {
                    usage,
                    down,
                    mods: 0,
                    owner_epoch: 1,
                    ctr: i + 1,
                })
                .unwrap()
            })
            .collect();

        // Warmup on a throwaway engine.
        let mut warm = granted_target();
        for p in payloads.iter().take(20_000) {
            black_box(warm.on_control(TYPE_KEY_EVENT, black_box(p)));
        }
        drop(warm);

        // Timed run on a fresh engine so ctr 1.. is accepted cleanly.
        let mut t = granted_target();
        let t0 = Instant::now();
        for p in &payloads {
            black_box(t.on_control(TYPE_KEY_EVENT, black_box(p)));
        }
        t0.elapsed().as_nanos() as f64 / f64::from(ITERS)
    };

    println!("\n=== mouser-engine target dispatch microbench ===");
    println!("on_motion(Datagram)        : {motion_ns:8.1} ns/op  (anti-replay + action build)");
    println!("on_control(TYPE_KEY_EVENT) : {key_ns:8.1} ns/op  (CBOR decode + anti-replay + transition + rate-cap + action)");
    println!("================================================\n");

    assert!(
        motion_ns < 20_000.0,
        "on_motion = {motion_ns:.1} ns/op (>20us) — engine dispatch regression"
    );
    assert!(
        key_ns < 20_000.0,
        "on_control(key) = {key_ns:.1} ns/op (>20us) — engine dispatch regression"
    );
}
