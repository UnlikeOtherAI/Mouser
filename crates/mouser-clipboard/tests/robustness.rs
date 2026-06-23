use mouser_clipboard::{
    canonical, content_hash, ClipboardEngine, ClipboardSettings, LocalRepr, MemContentSource,
    MAX_APPLIED_CLIPS, MAX_DATA_CHUNK, PULL_STALL_TICKS,
};
use mouser_protocol::{ClipFormat, Os};

fn bytes(seed: u64, len: usize) -> Vec<u8> {
    let mut v = Vec::with_capacity(len);
    let mut x = seed.wrapping_add(0x9E37_79B9_7F4A_7C15);
    for _ in 0..len {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        v.push((x & 0xFF) as u8);
    }
    v
}

fn dev(b: u8) -> [u8; 32] {
    [b; 32]
}

fn source_for(reps: &[LocalRepr]) -> MemContentSource {
    let mut src = MemContentSource::new();
    for rep in reps {
        let canon = canonical(rep.format, &rep.bytes);
        let hash = content_hash(rep.format, &rep.bytes);
        src.insert(rep.format, hash, canon);
    }
    src
}

#[test]
fn tick_sweeps_stalled_pull_and_abort_api_clears_hash() {
    let a = ClipboardEngine::new(dev(1), Os::Linux, ClipboardSettings::default());
    let mut b = ClipboardEngine::new(dev(2), Os::Windows, ClipboardSettings::default());
    let reps = vec![LocalRepr::new(ClipFormat::Utf8Text, b"stalled".to_vec())];
    let offer = a.on_local_change(&reps).expect("offer");
    let hash = content_hash(ClipFormat::Utf8Text, b"stalled");

    b.on_offer(&offer, Os::Linux)
        .expect("offer ok")
        .expect("pull");
    assert!(b.is_pulling(&hash));
    assert_eq!(b.tick(PULL_STALL_TICKS), 1);
    assert!(!b.is_pulling(&hash));

    b.on_offer(&offer, Os::Linux)
        .expect("offer ok")
        .expect("pull after sweep");
    assert!(b.abort_pull(&hash));
    assert!(!b.is_pulling(&hash));

    b.on_offer(&offer, Os::Linux)
        .expect("offer ok")
        .expect("pull after abort");
    assert_eq!(b.abort_origin(dev(1)), 1);
    assert!(!b.is_pulling(&hash));
}

#[test]
fn same_hash_reconnect_resets_partial_reassembly_and_reissues_pull() {
    let a = ClipboardEngine::new(dev(1), Os::Linux, ClipboardSettings::default());
    let mut b = ClipboardEngine::new(dev(2), Os::Windows, ClipboardSettings::default());
    let png = bytes(42, MAX_DATA_CHUNK + 32);
    let reps = vec![LocalRepr::new(ClipFormat::Png, png.clone())];
    let offer = a.on_local_change(&reps).expect("offer");
    let pull = b.on_offer(&offer, Os::Linux).expect("ok").expect("pull");
    let hash = content_hash(ClipFormat::Png, &png);
    let chunks = a.on_pull(&pull, &source_for(&reps)).expect("chunks");

    assert!(b.on_data(&chunks[0]).expect("first chunk").is_none());
    assert!(b.progress(&hash).expect("progress").received_bytes > 0);

    let pull_again = b
        .on_offer(&offer, Os::Linux)
        .expect("same hash offer ok")
        .expect("same hash reissued");
    assert_eq!(pull_again.hash, pull.hash);
    assert_eq!(b.progress(&hash).expect("reset progress").received_bytes, 0);

    let chunks_again = a
        .on_pull(&pull_again, &source_for(&reps))
        .expect("fresh chunks");
    let mut applied = None;
    for chunk in &chunks_again {
        if let Some(clip) = b.on_data(chunk).expect("fresh data") {
            applied = Some(clip);
        }
    }
    assert_eq!(applied.expect("applied").bytes, png);
}

#[test]
fn oversized_preferred_rep_falls_back_to_small_enabled_representation() {
    let settings = ClipboardSettings {
        max_auto_sync_bytes: 1024,
        ..ClipboardSettings::default()
    };
    let a = ClipboardEngine::new(dev(1), Os::Linux, ClipboardSettings::default());
    let mut b = ClipboardEngine::new(dev(2), Os::Windows, settings);
    let reps = vec![
        LocalRepr::new(ClipFormat::Png, bytes(7, 4096)),
        LocalRepr::new(ClipFormat::Utf8Text, b"small fallback".to_vec()),
    ];

    let offer = a.on_local_change(&reps).expect("offer");
    let pull = b.on_offer(&offer, Os::Linux).expect("ok").expect("pull");
    assert_eq!(pull.format, ClipFormat::Utf8Text);
}

#[test]
fn prefer_native_enabled_mid_stream_drops_final_apply() {
    let initial = ClipboardSettings {
        prefer_native_apple: false,
        ..ClipboardSettings::default()
    };
    let a = ClipboardEngine::new(dev(1), Os::Macos, initial);
    let mut b = ClipboardEngine::new(dev(2), Os::Ios, initial);
    let png = bytes(9, MAX_DATA_CHUNK + 8);
    let reps = vec![LocalRepr::new(ClipFormat::Png, png.clone())];
    let offer = a.on_local_change(&reps).expect("offer");
    let pull = b.on_offer(&offer, Os::Macos).expect("ok").expect("pull");
    let chunks = a.on_pull(&pull, &source_for(&reps)).expect("chunks");
    let hash = content_hash(ClipFormat::Png, &png);

    for chunk in &chunks[..chunks.len() - 1] {
        assert!(b.on_data(chunk).expect("partial data").is_none());
    }
    b.set_settings(ClipboardSettings::default());
    assert_eq!(
        b.on_data(&chunks[chunks.len() - 1]).expect("final data"),
        None
    );
    assert!(!b.was_applied(&hash));
    assert!(!b.is_pulling(&hash));
}

#[test]
fn prefer_native_offer_clears_existing_pending_from_origin() {
    let initial = ClipboardSettings {
        prefer_native_apple: false,
        ..ClipboardSettings::default()
    };
    let a = ClipboardEngine::new(dev(1), Os::Macos, initial);
    let mut b = ClipboardEngine::new(dev(2), Os::Ios, initial);
    let reps = vec![LocalRepr::new(ClipFormat::Utf8Text, b"handoff".to_vec())];
    let offer = a.on_local_change(&reps).expect("offer");
    let hash = content_hash(ClipFormat::Utf8Text, b"handoff");

    b.on_offer(&offer, Os::Macos)
        .expect("offer ok")
        .expect("initial pull");
    assert!(b.is_pulling(&hash));

    b.set_settings(ClipboardSettings::default());
    assert!(b.on_offer(&offer, Os::Macos).expect("suppressed").is_none());
    assert!(!b.is_pulling(&hash));
}

#[test]
fn applied_loop_prevention_log_is_bounded_and_one_shot() {
    let a = ClipboardEngine::new(dev(1), Os::Linux, ClipboardSettings::default());
    let mut b = ClipboardEngine::new(dev(2), Os::Windows, ClipboardSettings::default());
    let first = b"clip-0".to_vec();
    let first_hash = content_hash(ClipFormat::Utf8Text, &first);
    let mut last_hash = first_hash;

    for i in 0..=MAX_APPLIED_CLIPS {
        let text = format!("clip-{i}").into_bytes();
        last_hash = content_hash(ClipFormat::Utf8Text, &text);
        let reps = vec![LocalRepr::new(ClipFormat::Utf8Text, text)];
        let offer = a.on_local_change(&reps).expect("offer");
        let pull = b.on_offer(&offer, Os::Linux).expect("ok").expect("pull");
        let chunks = a.on_pull(&pull, &source_for(&reps)).expect("chunks");
        for chunk in &chunks {
            let _ = b.on_data(chunk).expect("data");
        }
    }

    assert_eq!(b.applied_count(), MAX_APPLIED_CLIPS);
    assert!(!b.was_applied(&first_hash));
    assert!(b.was_applied(&last_hash));

    let latest_text = format!("clip-{MAX_APPLIED_CLIPS}").into_bytes();
    let latest = vec![LocalRepr::new(ClipFormat::Utf8Text, latest_text)];
    assert!(b.on_local_change(&latest).is_none());
    assert!(b.on_local_change(&latest).is_some());
}
