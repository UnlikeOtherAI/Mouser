//! Integration tests for the §7.2 shared-state CRDT: independent edits exchange
//! changes and converge; snapshots round-trip; concurrent layout edits resolve
//! deterministically by the `(layout_rev, editor)` LWW rule.

use mouser_state::{device_id_hex, DeviceInfo, InputPrefs, Monitor, SharedState, STATE_FMT};

/// A deterministic 32-byte device id seeded from a single byte.
fn dev(seed: u8) -> [u8; 32] {
    let mut id = [0u8; 32];
    for (i, b) in id.iter_mut().enumerate() {
        *b = seed.wrapping_add(i as u8);
    }
    id
}

fn mon(display_id: u32, x: i32, y: i32) -> Monitor {
    Monitor {
        display_id,
        x,
        y,
        w: 1920,
        h: 1080,
        scale_milli: 2000,
        rotation: 0,
    }
}

/// Apply every change `from` holds that `into` is missing, in the manner the
/// engine would (gossip a `StateRequest`/`StateChanges` exchange).
fn pull(into: &mut SharedState, from: &SharedState) {
    let missing = from.changes_since(&into.heads());
    into.apply_changes(&missing).expect("apply changes");
}

/// Bidirectional exchange until both sides hold each other's changes.
fn exchange(a: &mut SharedState, b: &mut SharedState) {
    pull(a, b);
    pull(b, a);
}

#[test]
fn new_doc_is_empty() {
    let s = SharedState::new();
    assert_eq!(s.layout_rev(), 0);
    assert!(s.device(&dev(1)).expect("device").is_none());
    assert!(s.layout(&dev(1)).expect("layout").is_empty());
    assert!(s.alias(&dev(1)).expect("alias").is_none());
    assert_eq!(s.input_prefs().expect("prefs"), InputPrefs::default());
    assert!(s.device_ids_hex().is_empty());
    assert_eq!(STATE_FMT, 1);
}

#[test]
fn distinct_replicas_have_distinct_actors() {
    let a = SharedState::new();
    let b = SharedState::new();
    assert_ne!(a.actor_hex(), b.actor_hex());
}

#[test]
fn device_round_trip() {
    let mut s = SharedState::new();
    let id = dev(7);
    let info = DeviceInfo {
        name: "Studio".into(),
        os: "macos".into(),
    };
    s.set_device(&id, &info).expect("set device");
    assert_eq!(s.device(&id).expect("device"), Some(info));
    assert_eq!(s.device_ids_hex(), vec![device_id_hex(&id)]);
}

#[test]
fn alias_and_input_prefs_round_trip() {
    let mut s = SharedState::new();
    let id = dev(3);
    s.set_alias(&id, "Battlestation").expect("set alias");
    assert_eq!(
        s.alias(&id).expect("alias").as_deref(),
        Some("Battlestation")
    );

    let prefs = InputPrefs {
        edge_dwell_ms: 120,
        lock_on_drag: true,
        cursor_accel: false,
        cmd_ctrl_swap: true,
        hotkeys: vec![
            ("panic".into(), "Ctrl+Alt+P".into()),
            (format!("jump:{}", device_id_hex(&id)), "Ctrl+1".into()),
        ],
    };
    s.set_input_prefs(&prefs).expect("set prefs");
    let mut got = s.input_prefs().expect("prefs");
    got.hotkeys.sort();
    let mut expect = prefs.clone();
    expect.hotkeys.sort();
    assert_eq!(got, expect);
}

#[test]
fn layout_round_trip_and_rev_bump() {
    let mut s = SharedState::new();
    let d = dev(1);
    let editor = dev(1);
    let monitors = vec![mon(1, 0, 0), mon(2, 1920, 0)];
    s.set_layout(&d, &editor, &monitors).expect("set layout");
    assert_eq!(s.layout(&d).expect("layout"), monitors);
    assert_eq!(s.layout_rev(), 1);
    assert_eq!(s.layout_editor_hex(), device_id_hex(&editor));

    // A second edit bumps the rev monotonically.
    s.set_layout(&d, &editor, &[mon(1, 0, 0)])
        .expect("set layout 2");
    assert_eq!(s.layout_rev(), 2);
}

#[test]
fn snapshot_round_trips() {
    let mut s = SharedState::new();
    let a = dev(10);
    let b = dev(20);
    s.set_device(
        &a,
        &DeviceInfo {
            name: "A".into(),
            os: "linux".into(),
        },
    )
    .expect("device a");
    s.set_layout(&b, &b, &[mon(5, -1920, 0)]).expect("layout b");
    s.set_alias(&a, "alpha").expect("alias");

    let bytes = s.snapshot();
    let loaded = SharedState::load(&bytes).expect("load");

    assert_eq!(
        loaded.device(&a).expect("device"),
        s.device(&a).expect("device")
    );
    assert_eq!(
        loaded.layout(&b).expect("layout"),
        s.layout(&b).expect("layout")
    );
    assert_eq!(loaded.layout_rev(), s.layout_rev());
    assert_eq!(
        loaded.alias(&a).expect("alias"),
        s.alias(&a).expect("alias")
    );
    assert_eq!(loaded.heads(), s.heads());
}

#[test]
fn load_rejects_garbage() {
    assert!(SharedState::load(&[0xde, 0xad, 0xbe, 0xef]).is_err());
}

#[test]
fn independent_edits_converge() {
    let mut a = SharedState::new();
    let mut b = SharedState::new();
    let da = dev(1);
    let db = dev(2);

    // Each replica edits disjoint devices/layouts independently.
    a.set_device(
        &da,
        &DeviceInfo {
            name: "Mac".into(),
            os: "macos".into(),
        },
    )
    .expect("a device");
    a.set_layout(&da, &da, &[mon(1, 0, 0)]).expect("a layout");

    b.set_device(
        &db,
        &DeviceInfo {
            name: "PC".into(),
            os: "windows".into(),
        },
    )
    .expect("b device");
    b.set_layout(&db, &db, &[mon(7, 1920, 0)])
        .expect("b layout");

    exchange(&mut a, &mut b);

    // Both see both devices and both layouts.
    for s in [&a, &b] {
        assert_eq!(
            s.device(&da).expect("device da").map(|d| d.os),
            Some("macos".to_owned())
        );
        assert_eq!(
            s.device(&db).expect("device db").map(|d| d.os),
            Some("windows".to_owned())
        );
        assert_eq!(s.layout(&da).expect("layout da"), vec![mon(1, 0, 0)]);
        assert_eq!(s.layout(&db).expect("layout db"), vec![mon(7, 1920, 0)]);
    }
    // Identical heads ⇒ byte-identical state.
    assert_eq!(a.heads(), b.heads());
    assert_eq!(a.layout_rev(), b.layout_rev());
}

#[test]
fn concurrent_layout_edits_resolve_by_lww_rule() {
    // Two replicas concurrently set the SAME device's layout at the same rev.
    // The `(layout_rev, editor)` tiebreak must pick the higher editor id, and
    // both replicas must converge to that choice deterministically.
    let target = dev(50);
    let low_editor = dev(1); // smaller device_id
    let high_editor = dev(200); // larger device_id
    assert!(device_id_hex(&high_editor) > device_id_hex(&low_editor));

    let mut a = SharedState::new();
    let mut b = SharedState::new();

    // Establish a shared baseline (rev 0, no layout) so the next edits are
    // genuinely concurrent (no causal dependency between them).
    exchange(&mut a, &mut b);

    a.set_layout(&target, &low_editor, &[mon(1, 0, 0)])
        .expect("a layout");
    b.set_layout(&target, &high_editor, &[mon(9, 4000, 0)])
        .expect("b layout");

    // Both edits land at rev 1 independently.
    assert_eq!(a.layout_rev(), 1);
    assert_eq!(b.layout_rev(), 1);

    exchange(&mut a, &mut b);

    // Deterministic winner: same rev ⇒ higher editor id wins on BOTH replicas.
    assert_eq!(a.layout_rev(), b.layout_rev());
    assert_eq!(a.layout_rev(), 1);
    assert_eq!(a.layout_editor_hex(), device_id_hex(&high_editor));
    assert_eq!(b.layout_editor_hex(), device_id_hex(&high_editor));
    assert_eq!(a.heads(), b.heads());
}

#[test]
fn higher_rev_wins_over_lower_rev() {
    // A replica that has applied more layout edits (higher rev) wins regardless
    // of editor id ordering.
    let target = dev(50);
    let mut a = SharedState::new();
    let mut b = SharedState::new();
    exchange(&mut a, &mut b);

    // a edits twice (rev 2), b once (rev 1) with the LARGER editor id.
    a.set_layout(&target, &dev(1), &[mon(1, 0, 0)]).expect("a1");
    a.set_layout(&target, &dev(1), &[mon(2, 0, 0)]).expect("a2");
    b.set_layout(&target, &dev(255), &[mon(3, 0, 0)])
        .expect("b1");

    assert_eq!(a.layout_rev(), 2);
    assert_eq!(b.layout_rev(), 1);

    exchange(&mut a, &mut b);

    // rev 2 dominates rev 1 even though b's editor id is larger.
    assert_eq!(a.layout_rev(), 2);
    assert_eq!(b.layout_rev(), 2);
    assert_eq!(a.layout_editor_hex(), device_id_hex(&dev(1)));
    assert_eq!(b.layout_editor_hex(), device_id_hex(&dev(1)));
    assert_eq!(a.heads(), b.heads());
}

#[test]
fn changes_apply_in_any_order() {
    // apply_changes must tolerate out-of-causal-order delivery (automerge buffers
    // changes whose deps are unmet and applies them once parents arrive).
    let mut source = SharedState::new();
    let d = dev(11);
    source
        .set_device(
            &d,
            &DeviceInfo {
                name: "one".into(),
                os: "linux".into(),
            },
        )
        .expect("c1");
    source.set_layout(&d, &d, &[mon(1, 0, 0)]).expect("c2");
    source.set_layout(&d, &d, &[mon(2, 0, 0)]).expect("c3");

    let mut changes = source.changes_since(&[]);
    assert!(changes.len() >= 2, "expected multiple changes");
    changes.reverse(); // deliver newest-first (deps unmet)

    let mut sink = SharedState::new();
    sink.apply_changes(&changes).expect("apply reversed");

    assert_eq!(sink.layout(&d).expect("layout"), vec![mon(2, 0, 0)]);
    assert_eq!(sink.layout_rev(), source.layout_rev());
    assert_eq!(sink.heads(), source.heads());
}

#[test]
fn merge_matches_change_exchange() {
    let mut a = SharedState::new();
    let mut b = SharedState::new();
    a.set_alias(&dev(1), "a-side").expect("alias a");
    b.set_alias(&dev(2), "b-side").expect("alias b");

    a.merge(&b).expect("merge b into a");
    b.merge(&a).expect("merge a into b");

    assert_eq!(a.alias(&dev(1)).expect("a1").as_deref(), Some("a-side"));
    assert_eq!(a.alias(&dev(2)).expect("a2").as_deref(), Some("b-side"));
    assert_eq!(a.heads(), b.heads());
}

#[test]
fn changes_since_returns_only_the_delta() {
    let mut s = SharedState::new();
    let baseline = s.heads();
    s.set_alias(&dev(1), "new").expect("alias");
    let delta = s.changes_since(&baseline);
    assert_eq!(delta.len(), 1, "exactly one change since baseline");
    // And nothing is missing once a peer at `baseline` applies it.
    let mut peer = SharedState::new();
    peer.apply_changes(&s.changes_since(&peer.heads()))
        .expect("apply");
    assert!(peer.missing_deps(&s.heads()).is_empty());
}
