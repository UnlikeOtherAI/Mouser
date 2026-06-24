//! Serialization microbenchmarks for the input hot path (motion postcard codec, key
//! CBOR codec, control-frame framing).
//!
//! Deliberately dependency-free (no criterion/divan) so the workspace stays lean and the
//! numbers run in plain `cargo test`. Each case warms up, then times a tight `black_box`
//! loop and prints ns/op; a deliberately *generous* upper-bound assert catches a gross
//! regression without being flaky across CI hardware. The real budget is far tighter —
//! these functions are expected to be tens to low-hundreds of nanoseconds, which is
//! ~10,000x below the ~3 ms LAN one-way and the repo's <=5 ms wired motion-to-injection
//! SLO (docs/architecture.md). The point is to prove serialization is not a lag source.
//!
//! Run with numbers visible:
//!   cargo test -p mouser-protocol --test perf_codec -- --nocapture

use std::hint::black_box;
use std::time::Instant;

use mouser_protocol::{
    decode_datagram, encode_frame, encode_motion, from_cbor, to_cbor, KeyEvent, PointerMotion,
    TYPE_KEY_EVENT,
};

/// Mean nanoseconds per call of `f`, after a warmup pass.
fn bench<F: FnMut()>(iters: u32, mut f: F) -> f64 {
    let warmup = (iters / 10).max(10_000);
    for _ in 0..warmup {
        f();
    }
    let t0 = Instant::now();
    for _ in 0..iters {
        f();
    }
    t0.elapsed().as_nanos() as f64 / f64::from(iters)
}

/// Realistic non-zero field values — postcard/CBOR varints widen with magnitude, so
/// benching all-zero structs would understate real wire cost.
fn sample_motion() -> PointerMotion {
    PointerMotion {
        owner_epoch: 7,
        seq: 123_456,
        display_id: 1,
        x: 1920,
        y: 1080,
    }
}

fn sample_key() -> KeyEvent {
    KeyEvent {
        usage: 0x1A,
        down: true,
        mods: 0b0000_0010,
        owner_epoch: 7,
        ctr: 987_654,
    }
}

#[test]
fn perf_codec_report() {
    const ITERS: u32 = 1_000_000;

    let m = sample_motion();
    let motion_bytes = encode_motion(&m).unwrap();
    let k = sample_key();
    let key_bytes = to_cbor(&k).unwrap();
    let frame_bytes = encode_frame(TYPE_KEY_EVENT, 0, &key_bytes).unwrap();

    let enc_motion = bench(ITERS, || {
        black_box(encode_motion(black_box(&m)).unwrap());
    });
    let dec_motion = bench(ITERS, || {
        black_box(decode_datagram(black_box(&motion_bytes)).unwrap());
    });
    let enc_key = bench(ITERS, || {
        black_box(to_cbor(black_box(&k)).unwrap());
    });
    let dec_key = bench(ITERS, || {
        let v: KeyEvent = from_cbor(black_box(&key_bytes)).unwrap();
        black_box(v);
    });
    let enc_frame = bench(ITERS, || {
        black_box(encode_frame(TYPE_KEY_EVENT, 0, black_box(&key_bytes)).unwrap());
    });

    println!("\n=== mouser-protocol serialization microbench (n={ITERS}/op) ===");
    println!(
        "wire sizes: motion={}B, key(CBOR)={}B, key-frame={}B",
        motion_bytes.len(),
        key_bytes.len(),
        frame_bytes.len()
    );
    println!("encode_motion   : {enc_motion:8.1} ns/op");
    println!("decode_datagram : {dec_motion:8.1} ns/op");
    println!("to_cbor(Key)    : {enc_key:8.1} ns/op");
    println!("from_cbor(Key)  : {dec_key:8.1} ns/op");
    println!("encode_frame    : {enc_frame:8.1} ns/op");
    println!("===============================================================\n");

    // Generous regression ceiling (~10us). Expectation is < 1us; this only trips on a
    // catastrophic slowdown and stays robust across slow CI runners.
    for (name, ns) in [
        ("encode_motion", enc_motion),
        ("decode_datagram", dec_motion),
        ("to_cbor", enc_key),
        ("from_cbor", dec_key),
        ("encode_frame", enc_frame),
    ] {
        assert!(
            ns < 10_000.0,
            "{name} = {ns:.1} ns/op exceeds the 10us regression ceiling — serialization is not supposed to be a lag source"
        );
    }
}
