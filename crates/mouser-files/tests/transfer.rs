//! End-to-end tests for the file-transfer engine (§7.8): a full multi-file transfer
//! driven through an **in-memory transport** (the `pump` helper relays the engines'
//! messages), a transfer that **drops a chunk and resumes** from the receiver's
//! committed offset, and **path-traversal rejection**.

use std::path::{Path, PathBuf};

use mouser_files::{
    sha256, FileError, MemSink, MemSource, Outbound, Receiver, ReceiverConfig, Sender,
};
use mouser_protocol::FileOffer;

/// Build deterministic pseudo-random bytes so content assertions are meaningful.
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

const Q: &str = "/tmp/mouser-quarantine"; // never touched on disk — MemSink is in-memory.

/// Relay every outbound message between a sender and a `MemSink`-backed receiver until
/// the transfer completes (the in-memory transport for these tests).
fn drive_until_complete(
    sender: &mut Sender<MemSource>,
    receiver: &mut Receiver<MemSink>,
) -> Result<(), FileError> {
    loop {
        let mut progressed = false;
        while let Some(chunk) = sender.poll_chunk()? {
            for out in receiver.on_chunk(&chunk)? {
                apply_to_sender(sender, &out)?;
            }
            progressed = true;
        }
        if sender.is_complete() && receiver.is_complete() {
            return Ok(());
        }
        if !progressed {
            return Err(FileError::Protocol("stalled before completion".into()));
        }
    }
}

fn apply_to_sender(sender: &mut Sender<MemSource>, out: &Outbound) -> Result<(), FileError> {
    match out {
        Outbound::Ack(ack) => sender.on_ack(ack),
        Outbound::Done(done) => sender.on_done(done),
        Outbound::Accept(_) | Outbound::Reject(_) => Ok(()),
    }
}

fn make_mem_sink(_idx: usize, _path: &Path) -> Result<MemSink, mouser_files::SinkError> {
    Ok(MemSink::new())
}

#[test]
fn full_multi_file_transfer_succeeds_with_hash() {
    let f0 = bytes(1, 600_000); // > one 256 KiB chunk
    let f1 = bytes(2, 5); // tiny
    let f2 = bytes(3, 9_000_000); // > the 8 MiB window → forces windowed acks

    let mut sender = Sender::new(
        0xABCD,
        vec![
            ("alpha.bin".into(), MemSource::new(f0.clone())),
            ("beta.txt".into(), MemSource::new(f1.clone())),
            ("gamma.iso".into(), MemSource::new(f2.clone())),
        ],
    )
    .expect("sender");

    let expected = vec![Some(sha256(&f0)), Some(sha256(&f1)), Some(sha256(&f2))];
    let config = ReceiverConfig::new(PathBuf::from(Q)).with_expected_hashes(expected);
    let (mut receiver, accept) =
        Receiver::accept_offer(&sender.offer(), config, make_mem_sink).expect("offer accepted");
    let accept = match accept {
        Outbound::Accept(a) => a,
        other => panic!("expected Accept, got {other:?}"),
    };
    assert!(accept.resume.is_empty(), "fresh transfer has no resume points");
    sender.on_accept(&accept).expect("on_accept");

    drive_until_complete(&mut sender, &mut receiver).expect("transfer");

    assert!(sender.is_complete());
    assert!(receiver.is_complete());
    let states = receiver.states();
    assert_eq!(states.len(), 3);
    assert!(states.iter().all(|s| s.complete && s.acked == s.size));
}

/// A chunk is dropped mid-stream; the "connection breaks" and a fresh sender/receiver
/// pair resume from the bytes the receiver had already committed — bytes + hash must
/// still come out correct, and the second leg must carry a non-empty resume point.
#[test]
fn dropped_chunk_then_resume_completes_correctly() {
    let content = bytes(42, 1_000_000); // ~4 chunks at 256 KiB
    let transfer_id = 0x5151;

    // --- Leg 1: relay only the first two chunks, then "drop" the rest (break). ---
    let mut sender1 = Sender::new(
        transfer_id,
        vec![("resume-me.dat".into(), MemSource::new(content.clone()))],
    )
    .expect("sender1");
    let config1 = ReceiverConfig::new(PathBuf::from(Q));
    let (mut receiver1, accept1) =
        Receiver::accept_offer(&sender1.offer(), config1, make_mem_sink).expect("offer1");
    if let Outbound::Accept(a) = &accept1 {
        sender1.on_accept(a).expect("accept1");
    }

    let mut committed: Vec<u8> = Vec::new();
    let mut delivered = 0;
    while let Some(chunk) = sender1.poll_chunk().expect("poll1") {
        delivered += 1;
        // Commit two chunks, then simulate the link dropping the 3rd onward.
        if delivered > 2 {
            break;
        }
        for out in receiver1.on_chunk(&chunk).expect("chunk1") {
            if let Outbound::Ack(ack) = out {
                sender1.on_ack(&ack).expect("ack1");
            }
        }
        committed.extend_from_slice(&chunk.data);
    }
    let partial = receiver1.states();
    let resume_offset = partial.first().expect("file").acked;
    assert!(resume_offset > 0 && resume_offset < content.len() as u64);
    assert_eq!(&committed[..], &content[..resume_offset as usize]);

    // --- Leg 2: brand-new engines; receiver's sink already holds the committed prefix
    //     (this is exactly what a disk-backed sink would report via existing_len). ---
    let mut sender2 = Sender::new(
        transfer_id,
        vec![("resume-me.dat".into(), MemSource::new(content.clone()))],
    )
    .expect("sender2");
    let prefix = committed.clone();
    let make_resuming_sink =
        move |_i: usize, _p: &Path| Ok(MemSink::with_prefix(prefix.clone()));
    let config2 =
        ReceiverConfig::new(PathBuf::from(Q)).with_expected_hashes(vec![Some(sha256(&content))]);
    let (mut receiver2, accept2) =
        Receiver::accept_offer(&sender2.offer(), config2, make_resuming_sink).expect("offer2");
    let accept2 = match accept2 {
        Outbound::Accept(a) => a,
        other => panic!("expected Accept, got {other:?}"),
    };
    assert_eq!(
        accept2.resume,
        vec![mouser_protocol::ResumePoint {
            file_index: 0,
            offset: resume_offset
        }],
        "resume point must point at the committed prefix"
    );
    sender2.on_accept(&accept2).expect("accept2");

    drive_until_complete(&mut sender2, &mut receiver2).expect("resume transfer");

    assert!(receiver2.is_complete());
    // The reassembled bytes + hash are correct end to end despite the mid-stream drop.
    let states = receiver2.states();
    assert_eq!(states[0].acked, content.len() as u64);
}

#[test]
fn path_traversal_offer_is_rejected() {
    // `../../.ssh/authorized_keys` must be rejected — the transfer never opens a sink.
    let offer = FileOffer {
        transfer_id: 9,
        files: vec![mouser_protocol::FileEntry {
            name: "../../.ssh/authorized_keys".into(),
            size: 32,
        }],
    };
    let config = ReceiverConfig::new(PathBuf::from(Q));
    let (_recv, out) = Receiver::accept_offer(&offer, config, |_, _| -> Result<MemSink, _> {
        panic!("sink must NOT be created for an unsafe name")
    })
    .expect("accept_offer returns a reject, not an error");
    match out {
        Outbound::Reject(r) => {
            assert_eq!(r.transfer_id, 9);
            assert!(r.reason.contains("unsafe file name"), "reason: {}", r.reason);
        }
        other => panic!("expected Reject, got {other:?}"),
    }
}

#[test]
fn absolute_path_offer_is_rejected() {
    let offer = FileOffer {
        transfer_id: 10,
        files: vec![mouser_protocol::FileEntry {
            name: "/etc/passwd".into(),
            size: 1,
        }],
    };
    let config = ReceiverConfig::new(PathBuf::from(Q));
    let (_r, out) =
        Receiver::accept_offer(&offer, config, |_, _| -> Result<MemSink, _> {
            panic!("no sink for abs path")
        })
        .expect("ok");
    assert!(matches!(out, Outbound::Reject(_)));
}

#[test]
fn oversize_chunk_is_rejected_before_write() {
    let mut sender = Sender::new(11, vec![("x".into(), MemSource::new(bytes(7, 16)))]).expect("s");
    let config = ReceiverConfig::new(PathBuf::from(Q));
    let (mut receiver, accept) =
        Receiver::accept_offer(&sender.offer(), config, make_mem_sink).expect("offer");
    if let Outbound::Accept(a) = accept {
        sender.on_accept(&a).unwrap();
    }
    // Forge a chunk larger than the 1 MiB cap (§0.3) — rejected without allocation.
    let huge = mouser_protocol::FileChunk {
        transfer_id: 11,
        file_index: 0,
        offset: 0,
        data: vec![0u8; mouser_files::MAX_CHUNK_SIZE + 1],
    };
    assert_eq!(
        receiver.on_chunk(&huge),
        Err(FileError::ChunkTooLarge(mouser_files::MAX_CHUNK_SIZE + 1))
    );
}

#[test]
fn hash_mismatch_fails_the_transfer() {
    let content = bytes(99, 4_000);
    let mut sender =
        Sender::new(12, vec![("c.bin".into(), MemSource::new(content.clone()))]).expect("s");
    // Tell the receiver to expect the WRONG digest → completion must fail.
    let wrong = sha256(b"not the content");
    let config = ReceiverConfig::new(PathBuf::from(Q)).with_expected_hashes(vec![Some(wrong)]);
    let (mut receiver, accept) =
        Receiver::accept_offer(&sender.offer(), config, make_mem_sink).expect("offer");
    if let Outbound::Accept(a) = accept {
        sender.on_accept(&a).unwrap();
    }
    let err = drive_until_complete(&mut sender, &mut receiver).unwrap_err();
    assert_eq!(err, FileError::HashMismatch { file_index: 0 });
}
