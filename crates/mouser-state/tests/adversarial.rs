use automerge::transaction::Transactable;
use automerge::{AutoCommit, ObjType, ROOT};
use mouser_state::{
    device_id_hex, DeviceInfo, InputPrefs, Monitor, SharedState, CONTROL_WIRE_CAP,
    SNAPSHOT_WIRE_CAP,
};

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

fn compressed_chunk_marker() -> Vec<u8> {
    vec![0x85, 0x6f, 0x4a, 0x83, 0, 0, 0, 0, 2, 1, 0]
}

#[test]
fn oversize_garbage_and_compressed_inputs_are_rejected() {
    let mut state = SharedState::new();
    let before = state.heads();

    let oversized_change = vec![0u8; CONTROL_WIRE_CAP + 1];
    assert!(state.apply_changes(&[oversized_change]).is_err());
    assert!(state.apply_changes(&[vec![0xde, 0xad]]).is_err());
    assert!(state.apply_changes(&[compressed_chunk_marker()]).is_err());
    assert_eq!(state.heads(), before);

    let oversized_snapshot = vec![0u8; SNAPSHOT_WIRE_CAP + 1];
    assert!(SharedState::load(&oversized_snapshot).is_err());
    assert!(SharedState::load(&compressed_chunk_marker()).is_err());
}

#[test]
fn mixed_decode_errors_do_not_abort_valid_changes() {
    let mut source = SharedState::new();
    let id = dev(10);
    source.set_alias(&id, "valid").expect("alias");
    let mut batch = vec![vec![0xde, 0xad]];
    batch.extend(source.changes_since(&[]));
    batch.push(vec![0xba, 0xdd]);

    let mut sink = SharedState::new();
    sink.apply_changes(&batch).expect("mixed batch applies");

    assert_eq!(sink.alias(&id).expect("alias").as_deref(), Some("valid"));
}

#[test]
fn forged_duplicate_seq_change_is_skipped() {
    let mut real = SharedState::new();
    let mut forged = real.clone();
    let real_id = dev(1);
    let forged_id = dev(2);

    real.set_alias(&real_id, "real").expect("real alias");
    forged
        .set_alias(&forged_id, "forged")
        .expect("forged alias");

    let mut sink = SharedState::new();
    sink.apply_changes(&real.changes_since(&[]))
        .expect("real changes");
    sink.apply_changes(&forged.changes_since(&[]))
        .expect("forged duplicate skipped");

    assert_eq!(sink.alias(&real_id).expect("real").as_deref(), Some("real"));
    assert_eq!(sink.alias(&forged_id).expect("forged"), None);
}

#[test]
fn three_replicas_converge_with_duplicate_reordered_forged_batches() {
    let target = dev(50);
    let high_editor = dev(200);
    let low_editor = dev(1);
    let mut replicas = vec![SharedState::new(), SharedState::new(), SharedState::new()];

    replicas[0]
        .set_device(
            &low_editor,
            &DeviceInfo {
                name: "left".into(),
                os: "macos".into(),
            },
        )
        .expect("device");
    replicas[0]
        .set_layout(&target, &low_editor, &[mon(1, 0, 0)])
        .expect("low layout");
    replicas[1]
        .set_layout(&target, &high_editor, &[mon(9, 4000, 0)])
        .expect("high layout");
    replicas[1].set_alias(&target, "target").expect("alias");
    replicas[2]
        .set_input_prefs(&InputPrefs {
            edge_dwell_ms: 140,
            lock_on_drag: true,
            cursor_accel: true,
            cmd_ctrl_swap: false,
            hotkeys: vec![("panic".into(), "Ctrl+Alt+P".into())],
        })
        .expect("prefs");

    let batches: Vec<Vec<Vec<u8>>> = replicas.iter().map(|r| r.changes_since(&[])).collect();
    for replica in &mut replicas {
        for (idx, batch) in batches.iter().enumerate().rev() {
            let mut adversarial = batch.clone();
            adversarial.reverse();
            if let Some(first) = batch.first() {
                adversarial.push(first.clone());
            }
            adversarial.push(vec![0xde, idx as u8]);
            replica
                .apply_changes(&adversarial)
                .expect("adversarial batch");
        }
    }

    let heads = replicas[0].heads();
    for replica in &replicas {
        assert_eq!(replica.heads(), heads);
        assert_eq!(
            replica.layout(&target).expect("layout"),
            vec![mon(9, 4000, 0)]
        );
        assert_eq!(replica.layout_editor_hex(), device_id_hex(&high_editor));
        assert_eq!(
            replica.alias(&target).expect("alias").as_deref(),
            Some("target")
        );
        assert_eq!(replica.input_prefs().expect("prefs").edge_dwell_ms, 140);
    }
}

#[test]
fn snapshot_without_shared_genesis_is_rejected() {
    let mut doc = AutoCommit::new();
    doc.put_object(ROOT, "devices", ObjType::Map)
        .expect("foreign map");
    doc.commit();

    assert!(SharedState::load(&doc.save_nocompress()).is_err());
}

#[test]
fn poisoned_layout_rev_is_clamped_to_causal_history() {
    let base = SharedState::new();
    let mut doc = AutoCommit::load(&base.snapshot()).expect("load base");
    let poison = format!("{:020}|{}", u64::MAX, device_id_hex(&dev(250)));
    doc.put(ROOT, "layout_lww", poison.as_str())
        .expect("poison lww");
    doc.commit();

    let mut loaded = SharedState::load(&doc.save_nocompress()).expect("load poisoned");
    let clamped = loaded.layout_rev();
    assert!(clamped < u64::MAX);

    loaded
        .set_layout(&dev(3), &dev(3), &[mon(3, 0, 0)])
        .expect("local layout after poison");
    assert!(loaded.layout_rev() > clamped);
}
