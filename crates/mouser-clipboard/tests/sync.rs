//! End-to-end tests for the clipboard sync engine (§7.7): a two-device exchange driven
//! through an **in-memory relay** (no OS clipboard, no socket), covering the offer →
//! eager-pull → data round-trip for both a small text payload (single control message)
//! and a large image payload (multi-chunk bulk), prefer-native suppression, loop
//! prevention, progress reporting, hash-mismatch drop, and the settings gates.

use mouser_clipboard::{
    content_hash, AppliedClip, ClipboardEngine, ClipboardError, ClipboardSettings, LocalRepr,
    MemContentSource, SyncDirection, CONTROL_TEXT_CAP, MAX_DATA_CHUNK,
};
use mouser_protocol::{ClipFormat, ClipboardData, Os};

/// Deterministic pseudo-random bytes so content + hash assertions are meaningful.
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

/// Build a content source holding the canonical bytes for every offered representation
/// (what a real adapter snapshots when it builds the offer).
fn source_for(reps: &[LocalRepr]) -> MemContentSource {
    let mut src = MemContentSource::new();
    for rep in reps {
        let canon = mouser_clipboard::canonical(rep.format, &rep.bytes);
        let hash = content_hash(rep.format, &rep.bytes);
        src.insert(rep.format, hash, canon);
    }
    src
}

/// Relay a full clipboard sync from `sender`'s `reps` to `receiver` and return the
/// applied clip the receiver surfaces. Panics if no offer/pull is produced (callers
/// that expect suppression don't use this helper).
fn relay(
    sender: &ClipboardEngine,
    receiver: &mut ClipboardEngine,
    reps: &[LocalRepr],
    peer_is_sender_os: Os,
    receiver_sees_peer_os: Os,
) -> AppliedClip {
    let _ = peer_is_sender_os;
    let offer = sender.on_local_change(reps).expect("offer produced");
    let pull = receiver
        .on_offer(&offer, receiver_sees_peer_os)
        .expect("on_offer ok")
        .expect("pull produced");
    let src = source_for(reps);
    let chunks = sender.on_pull(&pull, &src).expect("on_pull ok");
    assert!(!chunks.is_empty(), "pull must yield data");
    let mut applied = None;
    for chunk in &chunks {
        if let Some(a) = receiver.on_data(chunk).expect("on_data ok") {
            applied = Some(a);
        }
    }
    applied.expect("a completed clip")
}

#[test]
fn small_text_offer_pull_data_round_trip_single_message() {
    let a = ClipboardEngine::new(dev(1), Os::Linux, ClipboardSettings::default());
    let mut b = ClipboardEngine::new(dev(2), Os::Windows, ClipboardSettings::default());

    // Raw text with CRLF — the applied bytes must be the canonical (LF) form.
    let reps = vec![LocalRepr::new(
        ClipFormat::Utf8Text,
        b"hello\r\nworld".to_vec(),
    )];

    let offer = a.on_local_change(&reps).expect("offer");
    assert_eq!(offer.origin, dev(1).to_vec());
    assert_eq!(offer.entries.len(), 1);
    assert_eq!(offer.entries[0].format, ClipFormat::Utf8Text);
    // size is the canonical size (CRLF collapsed).
    assert_eq!(offer.entries[0].size, b"hello\nworld".len() as u64);

    let pull = b.on_offer(&offer, Os::Linux).expect("ok").expect("pull");
    assert_eq!(pull.format, ClipFormat::Utf8Text);
    assert!(b.is_pulling(&content_hash(ClipFormat::Utf8Text, b"hello\r\nworld")));

    let src = source_for(&reps);
    let chunks = a.on_pull(&pull, &src).expect("on_pull");
    // Small payload ⇒ exactly one control-stream message, offset 0, last true.
    assert_eq!(chunks.len(), 1);
    assert_eq!(chunks[0].offset, 0);
    assert!(chunks[0].last);

    let applied = b.on_data(&chunks[0]).expect("on_data").expect("applied");
    assert_eq!(applied.format, ClipFormat::Utf8Text);
    assert_eq!(applied.bytes, b"hello\nworld"); // canonical bytes
}

#[test]
fn large_image_payload_streams_in_multiple_one_mib_chunks() {
    let a = ClipboardEngine::new(dev(1), Os::Linux, ClipboardSettings::default());
    let mut b = ClipboardEngine::new(dev(2), Os::Windows, ClipboardSettings::default());

    // > 2 MiB of "png" so it splits into >2 chunks of ≤ 1 MiB.
    let png = bytes(7, MAX_DATA_CHUNK * 2 + 12_345);
    let reps = vec![LocalRepr::new(ClipFormat::Png, png.clone())];

    let offer = a.on_local_change(&reps).expect("offer");
    assert_eq!(offer.entries[0].size, png.len() as u64);

    let pull = b.on_offer(&offer, Os::Linux).expect("ok").expect("pull");
    let src = source_for(&reps);
    let chunks = a.on_pull(&pull, &src).expect("on_pull");

    assert!(
        chunks.len() >= 3,
        "expected multi-chunk, got {}",
        chunks.len()
    );
    // Every non-final chunk is exactly 1 MiB; offsets are contiguous; only the last is
    // flagged `last`.
    let mut expected_offset = 0u64;
    for (i, c) in chunks.iter().enumerate() {
        assert_eq!(c.offset, expected_offset, "chunk {i} offset");
        assert!(c.data.len() <= MAX_DATA_CHUNK);
        let is_last = i == chunks.len() - 1;
        assert_eq!(c.last, is_last, "chunk {i} last flag");
        if !is_last {
            assert_eq!(c.data.len(), MAX_DATA_CHUNK, "non-final chunk is full");
        }
        expected_offset += c.data.len() as u64;
    }

    // Drive reassembly, checking progress climbs and only the final chunk completes.
    let hash = content_hash(ClipFormat::Png, &png);
    let mut applied = None;
    let mut last_seen = 0u64;
    for (i, c) in chunks.iter().enumerate() {
        let out = b.on_data(c).expect("on_data");
        if i < chunks.len() - 1 {
            assert!(out.is_none(), "non-final chunk must not complete");
            let p = b.progress(&hash).expect("progress while pending");
            assert!(p.received_bytes > last_seen, "progress advances");
            last_seen = p.received_bytes;
            assert!(!p.is_complete());
        } else {
            applied = out;
        }
    }
    let applied = applied.expect("final chunk completes");
    assert_eq!(applied.format, ClipFormat::Png);
    assert_eq!(applied.bytes, png);
    // Pull slot cleared once applied.
    assert!(!b.is_pulling(&hash));
    assert!(b.progress(&hash).is_none());
}

#[test]
fn eager_pull_picks_png_over_text_for_an_image_copy() {
    let a = ClipboardEngine::new(dev(1), Os::Linux, ClipboardSettings::default());
    let mut b = ClipboardEngine::new(dev(2), Os::Windows, ClipboardSettings::default());

    // An image copy commonly carries both a PNG and a text fallback; the puller must
    // choose PNG (preference order).
    let reps = vec![
        LocalRepr::new(ClipFormat::Utf8Text, b"https://example.com/img".to_vec()),
        LocalRepr::new(ClipFormat::Png, bytes(3, 4096)),
    ];
    let offer = a.on_local_change(&reps).expect("offer");
    let pull = b.on_offer(&offer, Os::Linux).expect("ok").expect("pull");
    assert_eq!(pull.format, ClipFormat::Png, "PNG beats text");
}

#[test]
fn eager_pull_picks_rich_text_over_plain() {
    let a = ClipboardEngine::new(dev(1), Os::Linux, ClipboardSettings::default());
    let mut b = ClipboardEngine::new(dev(2), Os::Windows, ClipboardSettings::default());
    let reps = vec![
        LocalRepr::new(ClipFormat::Utf8Text, b"plain".to_vec()),
        LocalRepr::new(ClipFormat::Html, b"<b>rich</b>".to_vec()),
    ];
    let offer = a.on_local_change(&reps).expect("offer");
    let pull = b.on_offer(&offer, Os::Linux).expect("ok").expect("pull");
    assert_eq!(pull.format, ClipFormat::Html, "html beats utf8_text");
}

#[test]
fn prefer_native_suppresses_apple_to_apple_only() {
    // Mac → iOS with prefer-native on: the receiver emits NO pull (OS handles it).
    let a = ClipboardEngine::new(dev(1), Os::Macos, ClipboardSettings::default());
    let mut ios = ClipboardEngine::new(dev(2), Os::Ios, ClipboardSettings::default());
    let reps = vec![LocalRepr::new(ClipFormat::Utf8Text, b"native".to_vec())];
    let offer = a.on_local_change(&reps).expect("offer still built");
    assert!(
        ios.on_offer(&offer, Os::Macos).expect("ok").is_none(),
        "apple↔apple is suppressed"
    );

    // Same offer to a Windows peer in the cluster is NOT suppressed (per-pair rule).
    let mut win = ClipboardEngine::new(dev(3), Os::Windows, ClipboardSettings::default());
    assert!(
        win.on_offer(&offer, Os::Macos).expect("ok").is_some(),
        "apple↔windows still uses Mouser"
    );
}

#[test]
fn prefer_native_off_does_not_suppress() {
    let settings = ClipboardSettings {
        prefer_native_apple: false,
        ..ClipboardSettings::default()
    };
    let a = ClipboardEngine::new(dev(1), Os::Macos, settings);
    let mut ios = ClipboardEngine::new(dev(2), Os::Ios, settings);
    let reps = vec![LocalRepr::new(ClipFormat::Utf8Text, b"x".to_vec())];
    let offer = a.on_local_change(&reps).expect("offer");
    assert!(
        ios.on_offer(&offer, Os::Macos).expect("ok").is_some(),
        "with prefer-native off, even apple↔apple syncs via Mouser"
    );
}

#[test]
fn applied_clip_is_not_reoffered_loop_prevention() {
    let a = ClipboardEngine::new(dev(1), Os::Linux, ClipboardSettings::default());
    let mut b = ClipboardEngine::new(dev(2), Os::Windows, ClipboardSettings::default());
    let reps = vec![LocalRepr::new(
        ClipFormat::Utf8Text,
        b"shared text".to_vec(),
    )];

    let applied = relay(&a, &mut b, &reps, Os::Linux, Os::Linux);
    assert_eq!(applied.bytes, b"shared text");

    // B's OS clipboard now holds exactly what it applied. When B's clipboard-change
    // watcher fires with that content, B must NOT offer it back (would loop to A).
    let bs_reps = vec![LocalRepr::new(applied.format, applied.bytes.clone())];
    assert!(
        b.on_local_change(&bs_reps).is_none(),
        "an applied clip must not be re-offered"
    );
    assert!(b.was_applied(&content_hash(applied.format, &applied.bytes)));

    // But genuinely new local content from B *is* offered.
    let fresh = vec![LocalRepr::new(
        ClipFormat::Utf8Text,
        b"B's own copy".to_vec(),
    )];
    assert!(b.on_local_change(&fresh).is_some());
}

#[test]
fn hash_mismatch_on_last_chunk_drops_and_errors() {
    let a = ClipboardEngine::new(dev(1), Os::Linux, ClipboardSettings::default());
    let mut b = ClipboardEngine::new(dev(2), Os::Windows, ClipboardSettings::default());
    let reps = vec![LocalRepr::new(ClipFormat::Utf8Text, b"correct".to_vec())];
    let offer = a.on_local_change(&reps).expect("offer");
    let pull = b.on_offer(&offer, Os::Linux).expect("ok").expect("pull");
    let hash = content_hash(ClipFormat::Utf8Text, b"correct");
    assert!(b.is_pulling(&hash));

    // Forge a data message: right hash + size, but corrupted bytes of the same length.
    let corrupt = ClipboardData {
        hash: pull.hash.clone(),
        format: ClipFormat::Utf8Text,
        offset: 0,
        data: b"corrupt".to_vec(), // same length as "correct"
        last: true,
    };
    assert_eq!(b.on_data(&corrupt), Err(ClipboardError::HashMismatch));
    // Pending payload dropped — the indicator clears, nothing applied.
    assert!(!b.is_pulling(&hash));
    assert!(!b.was_applied(&hash));
}

#[test]
fn data_for_unknown_hash_is_rejected() {
    let mut b = ClipboardEngine::new(dev(2), Os::Windows, ClipboardSettings::default());
    let stray = ClipboardData {
        hash: [9u8; 32].to_vec(),
        format: ClipFormat::Utf8Text,
        offset: 0,
        data: b"x".to_vec(),
        last: true,
    };
    assert_eq!(b.on_data(&stray), Err(ClipboardError::UnknownHash));
}

#[test]
fn master_switch_off_blocks_offer_and_pull() {
    let off = ClipboardSettings {
        shared_clipboard: false,
        ..ClipboardSettings::default()
    };
    let a = ClipboardEngine::new(dev(1), Os::Linux, off);
    let mut b = ClipboardEngine::new(dev(2), Os::Windows, off);
    let reps = vec![LocalRepr::new(ClipFormat::Utf8Text, b"x".to_vec())];
    // No offer when the master switch is off.
    assert!(a.on_local_change(&reps).is_none());
    // And an inbound offer (from some other device) is ignored.
    let on = ClipboardEngine::new(dev(3), Os::Linux, ClipboardSettings::default());
    let offer = on.on_local_change(&reps).expect("offer");
    assert!(b.on_offer(&offer, Os::Linux).expect("ok").is_none());
}

#[test]
fn direction_send_only_offers_but_never_pulls() {
    let send_only = ClipboardSettings {
        direction: SyncDirection::SendOnly,
        ..ClipboardSettings::default()
    };
    let dev_so = ClipboardEngine::new(dev(1), Os::Linux, send_only);
    let mut recv = dev_so; // reuse: same settings
    let reps = vec![LocalRepr::new(ClipFormat::Utf8Text, b"x".to_vec())];
    assert!(
        recv.on_local_change(&reps).is_some(),
        "send-only still offers"
    );

    let peer = ClipboardEngine::new(dev(2), Os::Linux, ClipboardSettings::default());
    let offer = peer.on_local_change(&reps).expect("offer");
    assert!(
        recv.on_offer(&offer, Os::Linux).expect("ok").is_none(),
        "send-only never pulls"
    );
}

#[test]
fn direction_receive_only_pulls_but_never_offers() {
    let recv_only = ClipboardSettings {
        direction: SyncDirection::ReceiveOnly,
        ..ClipboardSettings::default()
    };
    let mut dev_ro = ClipboardEngine::new(dev(1), Os::Linux, recv_only);
    let reps = vec![LocalRepr::new(ClipFormat::Utf8Text, b"x".to_vec())];
    assert!(
        dev_ro.on_local_change(&reps).is_none(),
        "receive-only never offers"
    );

    let peer = ClipboardEngine::new(dev(2), Os::Linux, ClipboardSettings::default());
    let offer = peer.on_local_change(&reps).expect("offer");
    assert!(
        dev_ro.on_offer(&offer, Os::Linux).expect("ok").is_some(),
        "receive-only pulls"
    );
}

#[test]
fn disabled_format_is_not_offered_and_not_pulled() {
    // Images off: a PNG-only copy yields no offer entry; an inbound PNG offer is not
    // pulled (and even if pulled, on_pull serves nothing).
    let no_images = ClipboardSettings {
        sync_images: false,
        ..ClipboardSettings::default()
    };
    let a = ClipboardEngine::new(dev(1), Os::Linux, no_images);
    let png = vec![LocalRepr::new(ClipFormat::Png, bytes(1, 64))];
    assert!(
        a.on_local_change(&png).is_none(),
        "png not offered when images off"
    );

    // Inbound PNG-only offer from a peer ⇒ no acceptable representation ⇒ no pull.
    let peer = ClipboardEngine::new(dev(2), Os::Linux, ClipboardSettings::default());
    let offer = peer.on_local_change(&png).expect("peer offers png");
    let mut b = ClipboardEngine::new(dev(3), Os::Windows, no_images);
    assert!(b.on_offer(&offer, Os::Linux).expect("ok").is_none());
}

#[test]
fn over_max_auto_sync_bytes_skips_eager_pull() {
    let small_limit = ClipboardSettings {
        max_auto_sync_bytes: 1024,
        ..ClipboardSettings::default()
    };
    let a = ClipboardEngine::new(dev(1), Os::Linux, ClipboardSettings::default());
    let mut b = ClipboardEngine::new(dev(2), Os::Windows, small_limit);

    // A 2 KiB payload exceeds the 1 KiB auto-sync limit ⇒ no eager pull.
    let big = vec![LocalRepr::new(ClipFormat::Png, bytes(5, 2048))];
    let offer = a.on_local_change(&big).expect("offer");
    assert!(b.on_offer(&offer, Os::Linux).expect("ok").is_none());

    // A payload exactly at the limit is allowed (inclusive boundary).
    let exact = vec![LocalRepr::new(ClipFormat::Png, bytes(5, 1024))];
    let offer2 = a.on_local_change(&exact).expect("offer2");
    assert!(b.on_offer(&offer2, Os::Linux).expect("ok").is_some());
}

#[test]
fn reflected_own_offer_is_not_pulled() {
    // An offer whose origin == self must never be pulled (no self-sync).
    let mut a = ClipboardEngine::new(dev(1), Os::Linux, ClipboardSettings::default());
    let reps = vec![LocalRepr::new(ClipFormat::Utf8Text, b"mine".to_vec())];
    let own = a.on_local_change(&reps).expect("offer");
    assert!(a.on_offer(&own, Os::Linux).expect("ok").is_none());
}

#[test]
fn uri_list_round_trip_canonicalizes() {
    let a = ClipboardEngine::new(dev(1), Os::Linux, ClipboardSettings::default());
    let mut b = ClipboardEngine::new(dev(2), Os::Windows, ClipboardSettings::default());
    // Trailing CRLF blank line must be canonicalized away (and bytes verify).
    let reps = vec![LocalRepr::new(
        ClipFormat::UriList,
        b"file:///a\r\nfile:///b\r\n".to_vec(),
    )];
    let applied = relay(&a, &mut b, &reps, Os::Linux, Os::Linux);
    assert_eq!(applied.format, ClipFormat::UriList);
    assert_eq!(applied.bytes, b"file:///a\nfile:///b");
}

#[test]
fn small_text_payload_is_a_single_control_message() {
    // A text payload within the control-stream cap rides as exactly one message
    // (offset 0, last true) — the §7.7 "instantly pasteable" small-text path.
    let a = ClipboardEngine::new(dev(1), Os::Linux, ClipboardSettings::default());
    let mut b = ClipboardEngine::new(dev(2), Os::Windows, ClipboardSettings::default());

    let at_cap = vec![LocalRepr::new(
        ClipFormat::Html,
        vec![b'x'; CONTROL_TEXT_CAP],
    )];
    let offer = a.on_local_change(&at_cap).expect("offer");
    let pull = b.on_offer(&offer, Os::Linux).expect("ok").expect("pull");
    let src = source_for(&at_cap);
    let chunks = a.on_pull(&pull, &src).expect("on_pull");
    assert_eq!(chunks.len(), 1, "at-cap payload is a single message");
    assert_eq!(chunks[0].offset, 0);
    assert!(chunks[0].last);
}

#[test]
fn chunk_count_splits_at_max_data_chunk_boundary() {
    // The chunk-count boundary is MAX_DATA_CHUNK (1 MiB): a payload exactly at it is a
    // single chunk; one byte over splits into two ordered chunks.
    let a = ClipboardEngine::new(dev(1), Os::Linux, ClipboardSettings::default());

    let mut b = ClipboardEngine::new(dev(2), Os::Windows, ClipboardSettings::default());
    let at = vec![LocalRepr::new(ClipFormat::Png, vec![b'x'; MAX_DATA_CHUNK])];
    let offer = a.on_local_change(&at).expect("offer");
    let pull = b.on_offer(&offer, Os::Linux).expect("ok").expect("pull");
    let chunks = a.on_pull(&pull, &source_for(&at)).expect("on_pull");
    assert_eq!(chunks.len(), 1, "exactly 1 MiB is a single chunk");
    assert!(chunks[0].last);

    let mut b2 = ClipboardEngine::new(dev(4), Os::Windows, ClipboardSettings::default());
    let over = vec![LocalRepr::new(
        ClipFormat::Png,
        vec![b'y'; MAX_DATA_CHUNK + 1],
    )];
    let offer2 = a.on_local_change(&over).expect("offer2");
    let pull2 = b2.on_offer(&offer2, Os::Linux).expect("ok").expect("pull2");
    let chunks2 = a.on_pull(&pull2, &source_for(&over)).expect("on_pull2");
    assert_eq!(
        chunks2.len(),
        2,
        "one byte over 1 MiB splits into two chunks"
    );
    assert_eq!(chunks2[0].data.len(), MAX_DATA_CHUNK);
    assert_eq!(chunks2[1].data.len(), 1);
    assert!(!chunks2[0].last && chunks2[1].last);
}

#[test]
fn pull_for_moved_on_content_yields_nothing() {
    // The source no longer holds the pulled hash (clipboard changed) ⇒ empty.
    let a = ClipboardEngine::new(dev(1), Os::Linux, ClipboardSettings::default());
    let reps = vec![LocalRepr::new(ClipFormat::Utf8Text, b"gone".to_vec())];
    let offer = a.on_local_change(&reps).expect("offer");
    let mut b = ClipboardEngine::new(dev(2), Os::Windows, ClipboardSettings::default());
    let pull = b.on_offer(&offer, Os::Linux).expect("ok").expect("pull");
    let empty_src = MemContentSource::new();
    assert!(a.on_pull(&pull, &empty_src).expect("on_pull").is_empty());
}
