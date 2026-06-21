//! End-to-end tests for the file-transfer engine (§7.8): a full multi-file transfer
//! driven through an **in-memory transport** (the `pump` helper relays the engines'
//! messages), a transfer that **drops a chunk and resumes** from the receiver's
//! committed offset, and **path-traversal rejection**.

use std::path::{Path, PathBuf};

use mouser_files::{
    sha256, FileError, FileSink, FileSource, Hash, MemSink, MemSource, Outbound, Receiver,
    ReceiverConfig, Sender, SinkError,
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

/// Relay every outbound message between a sender and receiver until the transfer
/// completes (the in-memory transport for these tests). Generic over the source/sink so
/// both the `MemSink` and disk-backed `FsSink` paths reuse it.
fn drive_until_complete<Src: FileSource, S: FileSink>(
    sender: &mut Sender<Src>,
    receiver: &mut Receiver<S>,
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

fn apply_to_sender<Src: FileSource>(
    sender: &mut Sender<Src>,
    out: &Outbound,
) -> Result<(), FileError> {
    match out {
        Outbound::Ack(ack) => sender.on_ack(ack),
        Outbound::Done(done) => sender.on_done(done),
        Outbound::Accept(_) | Outbound::Reject(_) => Ok(()),
    }
}

fn make_mem_sink(_idx: usize, _path: &Path) -> Result<MemSink, mouser_files::SinkError> {
    Ok(MemSink::new())
}

/// A test-only [`FileSink`] that buffers every write but then **fails to finalize**.
/// `existing_len()` is 0 (fresh), `write_at` succeeds, and `finish()` always returns an
/// `Err(SinkError)` — exercising the `finish()`-failure arm of the receiver's finalize
/// path (the same abort path as a hash mismatch, but reached via the sink rather than the
/// digest comparison).
#[derive(Default)]
struct FailFinishSink {
    bytes: Vec<u8>,
}

impl FileSink for FailFinishSink {
    fn existing_len(&self) -> u64 {
        self.bytes.len() as u64
    }

    fn write_at(&mut self, offset: u64, data: &[u8]) -> Result<(), SinkError> {
        if offset != self.existing_len() {
            return Err(SinkError(format!(
                "non-contiguous write at {offset}, have {}",
                self.bytes.len()
            )));
        }
        self.bytes.extend_from_slice(data);
        Ok(())
    }

    fn finish(&mut self) -> Result<Hash, SinkError> {
        Err(SinkError("simulated finish failure".into()))
    }
}

fn make_fail_finish_sink(_idx: usize, _path: &Path) -> Result<FailFinishSink, SinkError> {
    Ok(FailFinishSink::default())
}

/// Pull the `FileAccept` out of `accept_offer`'s outbound list (always the first message
/// for a non-rejected offer); panics with the actual variant otherwise.
fn expect_accept(out: Vec<Outbound>) -> mouser_protocol::FileAccept {
    match out.into_iter().next() {
        Some(Outbound::Accept(a)) => a,
        other => panic!("expected Accept first, got {other:?}"),
    }
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
    let (mut receiver, out) =
        Receiver::accept_offer(&sender.offer(), config, make_mem_sink).expect("offer accepted");
    let accept = expect_accept(out);
    assert!(
        accept.resume.is_empty(),
        "fresh transfer has no resume points"
    );
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
    let (mut receiver1, out1) =
        Receiver::accept_offer(&sender1.offer(), config1, make_mem_sink).expect("offer1");
    sender1.on_accept(&expect_accept(out1)).expect("accept1");

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
    let make_resuming_sink = move |_i: usize, _p: &Path| Ok(MemSink::with_prefix(prefix.clone()));
    let config2 =
        ReceiverConfig::new(PathBuf::from(Q)).with_expected_hashes(vec![Some(sha256(&content))]);
    let (mut receiver2, out2) =
        Receiver::accept_offer(&sender2.offer(), config2, make_resuming_sink).expect("offer2");
    let accept2 = expect_accept(out2);
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
            sha256: None,
        }],
    };
    let config = ReceiverConfig::new(PathBuf::from(Q));
    let (recv, out) = Receiver::accept_offer(&offer, config, |_, _| -> Result<MemSink, _> {
        panic!("sink must NOT be created for an unsafe name")
    })
    .expect("accept_offer returns a reject, not an error");
    assert!(
        recv.is_aborted(),
        "an unsafe-name offer aborts the transfer"
    );
    match out.as_slice() {
        [Outbound::Reject(r)] => {
            assert_eq!(r.transfer_id, 9);
            assert!(
                r.reason.contains("unsafe file name"),
                "reason: {}",
                r.reason
            );
        }
        other => panic!("expected a single Reject, got {other:?}"),
    }
}

#[test]
fn absolute_path_offer_is_rejected() {
    let offer = FileOffer {
        transfer_id: 10,
        files: vec![mouser_protocol::FileEntry {
            name: "/etc/passwd".into(),
            size: 1,
            sha256: None,
        }],
    };
    let config = ReceiverConfig::new(PathBuf::from(Q));
    let (_r, out) = Receiver::accept_offer(&offer, config, |_, _| -> Result<MemSink, _> {
        panic!("no sink for abs path")
    })
    .expect("ok");
    assert!(matches!(out.as_slice(), [Outbound::Reject(_)]));
}

#[test]
fn oversize_chunk_is_rejected_before_write() {
    let mut sender = Sender::new(11, vec![("x".into(), MemSource::new(bytes(7, 16)))]).expect("s");
    let config = ReceiverConfig::new(PathBuf::from(Q));
    let (mut receiver, out) =
        Receiver::accept_offer(&sender.offer(), config, make_mem_sink).expect("offer");
    sender.on_accept(&expect_accept(out)).unwrap();
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

/// A SHA-256 mismatch on the last chunk must surface as a `FileDone{ok:false}` on the
/// wire (not a swallowed local error), the receiver must NOT report the file complete,
/// and feeding that `FileDone` to the sender must mark *its* side aborted (§7.8).
#[test]
fn hash_mismatch_emits_filedone_not_ok_and_aborts_sender() {
    let content = bytes(99, 4_000);
    let transfer_id = 12;
    let mut sender = Sender::new(
        transfer_id,
        vec![("c.bin".into(), MemSource::new(content.clone()))],
    )
    .expect("s");
    // Tell the receiver to expect the WRONG digest → completion must fail.
    let wrong = sha256(b"not the content");
    let config = ReceiverConfig::new(PathBuf::from(Q)).with_expected_hashes(vec![Some(wrong)]);
    let (mut receiver, out) =
        Receiver::accept_offer(&sender.offer(), config, make_mem_sink).expect("offer");
    sender.on_accept(&expect_accept(out)).unwrap();

    // Relay every chunk; collect the receiver's outbound messages (acks + the terminal
    // done) without touching the sender, so we can inspect the exact wire message.
    let mut done: Option<mouser_protocol::FileDone> = None;
    while let Some(chunk) = sender.poll_chunk().expect("poll") {
        for o in receiver.on_chunk(&chunk).expect("chunk") {
            match o {
                Outbound::Ack(ack) => sender.on_ack(&ack).expect("ack"),
                Outbound::Done(d) => done = Some(d),
                other => panic!("unexpected outbound {other:?}"),
            }
        }
    }

    // 1) The receiver produced a `FileDone{ok:false}` on the wire for this transfer.
    assert_eq!(
        done,
        Some(mouser_protocol::FileDone {
            transfer_id,
            ok: false
        }),
        "a hash mismatch must emit FileDone{{ok:false}}"
    );
    // 2) The receiver did NOT commit the corrupt file as complete.
    assert!(receiver.is_aborted(), "receiver aborts on mismatch");
    assert!(
        !receiver.is_complete(),
        "a mismatched file is not 'complete'"
    );
    assert!(!receiver.states()[0].complete);
    // 3) Further chunks are ignored rather than resurrecting the transfer.
    let stray = mouser_protocol::FileChunk {
        transfer_id,
        file_index: 0,
        offset: 0,
        data: vec![0u8; 4],
    };
    assert!(receiver.on_chunk(&stray).expect("ignored").is_empty());

    // 4) The sender, on receiving that FileDone, marks ITS side aborted (not complete).
    sender.on_done(&done.unwrap()).expect("on_done");
    assert!(sender.is_aborted(), "sender aborts on FileDone{{ok:false}}");
    assert!(!sender.is_complete(), "an aborted sender is not complete");
}

/// A `sink.finish()` failure on the last chunk must surface as a `FileDone{ok:false}` on
/// the wire (not a swallowed local error), the receiver must NOT report the file complete,
/// and feeding that `FileDone` to the sender must mark *its* side aborted (§7.8). This is
/// the finalize-failure twin of `hash_mismatch_emits_filedone_not_ok_and_aborts_sender`:
/// it reaches the same abort path via the sink's `finish()` rather than a digest mismatch.
#[test]
fn finish_failure_emits_filedone_not_ok_and_aborts_sender() {
    let content = bytes(123, 4_000);
    let transfer_id = 13;
    let mut sender = Sender::new(
        transfer_id,
        vec![("c.bin".into(), MemSource::new(content.clone()))],
    )
    .expect("s");
    // No expected hash here: the abort must come purely from the sink's finish() failure,
    // not the digest comparison — so this independently covers the finish()-error arm.
    let config = ReceiverConfig::new(PathBuf::from(Q));
    let (mut receiver, out) =
        Receiver::accept_offer(&sender.offer(), config, make_fail_finish_sink).expect("offer");
    sender.on_accept(&expect_accept(out)).unwrap();

    // Relay every chunk; collect the receiver's outbound messages (acks + the terminal
    // done) without touching the sender, so we can inspect the exact wire message.
    let mut done: Option<mouser_protocol::FileDone> = None;
    while let Some(chunk) = sender.poll_chunk().expect("poll") {
        for o in receiver.on_chunk(&chunk).expect("chunk") {
            match o {
                Outbound::Ack(ack) => sender.on_ack(&ack).expect("ack"),
                Outbound::Done(d) => done = Some(d),
                other => panic!("unexpected outbound {other:?}"),
            }
        }
    }

    // 1) The receiver produced a `FileDone{ok:false}` on the wire for this transfer.
    assert_eq!(
        done,
        Some(mouser_protocol::FileDone {
            transfer_id,
            ok: false
        }),
        "a finish() failure must emit FileDone{{ok:false}}"
    );
    // 2) The receiver did NOT commit the file as complete.
    assert!(receiver.is_aborted(), "receiver aborts on finish() failure");
    assert!(
        !receiver.is_complete(),
        "a finalize-failed file is not 'complete'"
    );
    assert!(!receiver.states()[0].complete);
    // 3) Further chunks are ignored rather than resurrecting the transfer.
    let stray = mouser_protocol::FileChunk {
        transfer_id,
        file_index: 0,
        offset: 0,
        data: vec![0u8; 4],
    };
    assert!(receiver.on_chunk(&stray).expect("ignored").is_empty());

    // 4) The sender, on receiving that FileDone, marks ITS side aborted (not complete).
    sender.on_done(&done.unwrap()).expect("on_done");
    assert!(sender.is_aborted(), "sender aborts on FileDone{{ok:false}}");
    assert!(!sender.is_complete(), "an aborted sender is not complete");
}

/// A partial file on disk LONGER than the offer's declared `size` is corruption: the
/// receiver must REJECT the whole transfer rather than clamp the resume offset to
/// `size` and silently accept a too-long prefix.
#[test]
fn resume_with_existing_longer_than_size_is_rejected() {
    let offer = FileOffer {
        transfer_id: 77,
        files: vec![mouser_protocol::FileEntry {
            name: "grew.dat".into(),
            size: 100,
            sha256: None,
        }],
    };
    // The sink already holds MORE bytes than the offer claims (existing_len > size).
    let bloated = bytes(5, 250);
    let make_bloated = move |_i: usize, _p: &Path| Ok(MemSink::with_prefix(bloated.clone()));
    let config = ReceiverConfig::new(PathBuf::from(Q));
    let (recv, out) = Receiver::accept_offer(&offer, config, make_bloated).expect("ok");

    assert!(
        recv.is_aborted(),
        "an over-long existing file aborts the transfer"
    );
    match out.as_slice() {
        [Outbound::Reject(r)] => {
            assert_eq!(r.transfer_id, 77);
            assert!(
                r.reason.contains("exceed offered size"),
                "reason should explain the over-long prefix, got: {}",
                r.reason
            );
        }
        other => panic!("expected a single Reject for an over-long prefix, got {other:?}"),
    }
}

// --- C2-4: in-band digest is verified end-to-end -----------------------------------

/// The sender advertises each file's SHA-256 in the offer (`FileEntry.sha256`); the
/// receiver — given NO out-of-band hashes — adopts the in-band digest and verifies it on
/// completion. Correct content ⇒ the transfer completes with the digest checked.
#[test]
fn in_band_offer_digest_is_verified_on_completion() {
    let f0 = bytes(1, 600_000);
    let f1 = bytes(2, 4_000);
    let mut sender = Sender::new_with_hashes(
        0xD1,
        vec![
            (
                "a.bin".into(),
                MemSource::new(f0.clone()),
                Some(sha256(&f0)),
            ),
            (
                "b.bin".into(),
                MemSource::new(f1.clone()),
                Some(sha256(&f1)),
            ),
        ],
    )
    .expect("sender");

    // The offer carries the digests on the wire.
    let offer = sender.offer();
    assert_eq!(offer.files[0].sha256.as_deref(), Some(&sha256(&f0)[..]));
    assert_eq!(offer.files[1].sha256.as_deref(), Some(&sha256(&f1)[..]));

    // Receiver has NO out-of-band hashes — it must rely on the in-band ones.
    let config = ReceiverConfig::new(PathBuf::from(Q));
    let (mut receiver, out) = Receiver::accept_offer(&offer, config, make_mem_sink).expect("offer");
    sender.on_accept(&expect_accept(out)).expect("accept");
    drive_until_complete(&mut sender, &mut receiver).expect("transfer");
    assert!(receiver.is_complete());
    assert!(receiver.is_terminal());
}

/// A WRONG in-band digest (the offer claims a hash the bytes don't match) must abort with
/// `FileDone{ok:false}` — proving the receiver actually checks the in-band value.
#[test]
fn wrong_in_band_offer_digest_aborts() {
    let content = bytes(7, 4_000);
    let offer = FileOffer {
        transfer_id: 0xD2,
        files: vec![mouser_protocol::FileEntry {
            name: "c.bin".into(),
            size: content.len() as u64,
            sha256: Some(sha256(b"a different file").to_vec()),
        }],
    };
    let mut sender = Sender::new(
        0xD2,
        vec![("c.bin".into(), MemSource::new(content.clone()))],
    )
    .expect("s");
    let config = ReceiverConfig::new(PathBuf::from(Q));
    let (mut receiver, out) = Receiver::accept_offer(&offer, config, make_mem_sink).expect("offer");
    sender.on_accept(&expect_accept(out)).unwrap();

    let mut done = None;
    while let Some(chunk) = sender.poll_chunk().expect("poll") {
        for o in receiver.on_chunk(&chunk).expect("chunk") {
            match o {
                Outbound::Ack(a) => sender.on_ack(&a).expect("ack"),
                Outbound::Done(d) => done = Some(d),
                other => panic!("unexpected {other:?}"),
            }
        }
    }
    assert_eq!(
        done.map(|d| d.ok),
        Some(false),
        "wrong in-band digest aborts"
    );
    assert!(receiver.is_aborted());
}

/// An in-band `sha256` that is not exactly 32 bytes is malformed — the offer is rejected
/// before any sink opens.
#[test]
fn malformed_in_band_digest_is_rejected() {
    let offer = FileOffer {
        transfer_id: 0xD3,
        files: vec![mouser_protocol::FileEntry {
            name: "c.bin".into(),
            size: 4,
            sha256: Some(vec![0u8; 7]), // wrong length
        }],
    };
    let config = ReceiverConfig::new(PathBuf::from(Q));
    let (recv, out) = Receiver::accept_offer(&offer, config, |_, _| -> Result<MemSink, _> {
        panic!("no sink for malformed digest")
    })
    .expect("ok");
    assert!(recv.is_aborted());
    assert!(matches!(out.as_slice(), [Outbound::Reject(_)]));
}

// --- Receiver robustness: forward-gap, write failure, is_terminal ------------------

/// A forward-gap chunk (`offset > acked`) must be NON-fatal: the receiver re-acks the
/// contiguous prefix it actually holds (so the sender rewinds) instead of returning a
/// fatal `FileError` that tears the connection down (audit R2).
#[test]
fn forward_gap_chunk_is_reacked_not_fatal() {
    let mut sender =
        Sender::new(0xF0, vec![("x".into(), MemSource::new(bytes(7, 4_000)))]).expect("s");
    let config = ReceiverConfig::new(PathBuf::from(Q));
    let (mut receiver, out) =
        Receiver::accept_offer(&sender.offer(), config, make_mem_sink).expect("offer");
    sender.on_accept(&expect_accept(out)).unwrap();

    // Forge a chunk that starts past offset 0 while the receiver still holds 0 bytes.
    let gap = mouser_protocol::FileChunk {
        transfer_id: 0xF0,
        file_index: 0,
        offset: 1_000,
        data: vec![0u8; 16],
    };
    let out = receiver.on_chunk(&gap).expect("gap is non-fatal");
    match out.as_slice() {
        [Outbound::Ack(a)] => {
            assert_eq!(a.acked_through, 0, "re-acks the prefix actually held");
            assert_eq!(a.file_index, 0);
        }
        other => panic!("expected a single re-ack for a forward gap, got {other:?}"),
    }
    // The transfer is still live (not aborted) and can complete normally afterwards.
    assert!(!receiver.is_aborted());
    drive_until_complete(&mut sender, &mut receiver).expect("transfer still completes");
    assert!(receiver.is_complete());
}

/// A `write_at` failure (not just a hash/finish failure) must emit `FileDone{ok:false}`
/// to the peer and abort, rather than propagate a fatal error (audit R2).
#[test]
fn write_failure_emits_filedone_not_ok() {
    /// A sink whose `write_at` always fails.
    #[derive(Default)]
    struct FailWriteSink;
    impl FileSink for FailWriteSink {
        fn existing_len(&self) -> u64 {
            0
        }
        fn write_at(&mut self, _offset: u64, _data: &[u8]) -> Result<(), SinkError> {
            Err(SinkError("simulated write failure".into()))
        }
        fn finish(&mut self) -> Result<Hash, SinkError> {
            Ok([0u8; 32])
        }
    }

    let mut sender =
        Sender::new(0xF1, vec![("x".into(), MemSource::new(bytes(7, 4_000)))]).expect("s");
    let config = ReceiverConfig::new(PathBuf::from(Q));
    let (mut receiver, out) =
        Receiver::accept_offer(&sender.offer(), config, |_, _| Ok(FailWriteSink)).expect("offer");
    sender.on_accept(&expect_accept(out)).unwrap();

    let first = sender.poll_chunk().expect("poll").expect("a chunk");
    let out = receiver
        .on_chunk(&first)
        .expect("write failure is non-fatal");
    assert_eq!(
        out.as_slice(),
        [Outbound::Done(mouser_protocol::FileDone {
            transfer_id: 0xF1,
            ok: false
        })],
        "a write failure must surface as FileDone{{ok:false}}"
    );
    assert!(receiver.is_aborted());
    assert!(receiver.is_terminal(), "an aborted receiver is terminal");
    // Further chunks are ignored, not resurrecting the transfer.
    assert!(receiver.on_chunk(&first).expect("ignored").is_empty());
}

// --- Bounds: max_file_size / max_files / max_total_bytes ----------------------------

fn offer_n(transfer_id: u64, sizes: &[u64]) -> FileOffer {
    FileOffer {
        transfer_id,
        files: sizes
            .iter()
            .enumerate()
            .map(|(i, &size)| mouser_protocol::FileEntry {
                name: format!("f{i}.bin"),
                size,
                sha256: None,
            })
            .collect(),
    }
}

#[test]
fn offer_exceeding_bounds_is_rejected_before_opening_sinks() {
    let no_sink = |_: usize, _: &Path| -> Result<MemSink, SinkError> {
        panic!("no sink may open for an over-bounds offer")
    };

    // max_file_size.
    let cfg = ReceiverConfig::new(PathBuf::from(Q)).with_limits(Some(1_000), None, None);
    let (r, out) = Receiver::accept_offer(&offer_n(1, &[2_000]), cfg, no_sink).expect("ok");
    assert!(r.is_aborted());
    assert!(matches!(out.as_slice(), [Outbound::Reject(_)]));

    // max_files.
    let cfg = ReceiverConfig::new(PathBuf::from(Q)).with_limits(None, Some(2), None);
    let (r, out) = Receiver::accept_offer(&offer_n(2, &[1, 1, 1]), cfg, no_sink).expect("ok");
    assert!(r.is_aborted());
    assert!(matches!(out.as_slice(), [Outbound::Reject(_)]));

    // max_total_bytes.
    let cfg = ReceiverConfig::new(PathBuf::from(Q)).with_limits(None, None, Some(100));
    let (r, out) = Receiver::accept_offer(&offer_n(3, &[60, 60]), cfg, no_sink).expect("ok");
    assert!(r.is_aborted());
    assert!(matches!(out.as_slice(), [Outbound::Reject(_)]));
}

#[test]
fn offer_within_bounds_is_accepted() {
    let cfg = ReceiverConfig::new(PathBuf::from(Q)).with_limits(Some(1_000), Some(4), Some(2_000));
    let (r, out) =
        Receiver::accept_offer(&offer_n(4, &[500, 500]), cfg, make_mem_sink).expect("ok");
    assert!(!r.is_aborted());
    assert!(matches!(out.first(), Some(Outbound::Accept(_))));
}

// --- Sender resume-trust -----------------------------------------------------------

#[test]
fn sender_rejects_duplicate_resume_index() {
    let mut sender =
        Sender::new(0xA1, vec![("x".into(), MemSource::new(bytes(7, 4_000)))]).expect("s");
    let accept = mouser_protocol::FileAccept {
        transfer_id: 0xA1,
        resume: vec![
            mouser_protocol::ResumePoint {
                file_index: 0,
                offset: 10,
            },
            mouser_protocol::ResumePoint {
                file_index: 0,
                offset: 20,
            },
        ],
    };
    assert!(
        matches!(sender.on_accept(&accept), Err(FileError::Protocol(_))),
        "a duplicate resume file_index must be rejected"
    );
}

#[test]
fn sender_on_accept_is_single_shot() {
    let mut sender =
        Sender::new(0xA2, vec![("x".into(), MemSource::new(bytes(7, 4_000)))]).expect("s");
    let accept = mouser_protocol::FileAccept {
        transfer_id: 0xA2,
        resume: vec![],
    };
    sender.on_accept(&accept).expect("first accept");
    assert!(
        matches!(sender.on_accept(&accept), Err(FileError::Protocol(_))),
        "a second FileAccept must be rejected"
    );
}

#[test]
fn sender_rejects_resume_offset_past_size() {
    let mut sender =
        Sender::new(0xA3, vec![("x".into(), MemSource::new(bytes(7, 100)))]).expect("s");
    let accept = mouser_protocol::FileAccept {
        transfer_id: 0xA3,
        resume: vec![mouser_protocol::ResumePoint {
            file_index: 0,
            offset: 101,
        }],
    };
    assert!(
        matches!(
            sender.on_accept(&accept),
            Err(FileError::OffsetOutOfRange { .. })
        ),
        "a resume offset past size must be rejected"
    );
}

// --- C2-5: disk-backed FsSink resume + symlink safety ------------------------------

#[cfg(unix)]
mod disk {
    use super::*;
    use mouser_files::FsSink;

    /// A unique scratch dir under the system temp dir for one test (cleaned at the end).
    struct Scratch {
        dir: PathBuf,
    }
    impl Scratch {
        fn new(tag: &str) -> Self {
            use std::sync::atomic::{AtomicU64, Ordering};
            static COUNTER: AtomicU64 = AtomicU64::new(0);
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let dir = std::env::temp_dir().join(format!(
                "mouser-files-{}-{}-{tag}-{n}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_nanos())
                    .unwrap_or(0),
            ));
            std::fs::create_dir_all(&dir).expect("create scratch dir");
            Self { dir }
        }
        fn path(&self, name: &str) -> PathBuf {
            self.dir.join(name)
        }
    }
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    /// A full transfer landing on a real on-disk file via `FsSink`: bytes + streaming
    /// digest must match, and the file on disk must equal the source.
    #[test]
    fn fs_sink_full_transfer_writes_correct_bytes_and_hash() {
        let scratch = Scratch::new("full");
        let content = bytes(11, 700_000);
        let path = scratch.path("out.bin");
        let quarantine = scratch.dir.clone();

        let mut sender = Sender::new_with_hashes(
            0xC5,
            vec![(
                "out.bin".into(),
                MemSource::new(content.clone()),
                Some(sha256(&content)),
            )],
        )
        .expect("s");
        let make = move |_i: usize, p: &Path| FsSink::open(p);
        let config = ReceiverConfig::new(quarantine);
        let (mut receiver, out) =
            Receiver::accept_offer(&sender.offer(), config, make).expect("offer");
        sender.on_accept(&expect_accept(out)).expect("accept");
        drive_until_complete(&mut sender, &mut receiver).expect("transfer");

        assert!(receiver.is_complete());
        let on_disk = std::fs::read(&path).expect("read out.bin");
        assert_eq!(on_disk, content, "the file on disk equals the source");
    }

    /// The headline C2-5 case: a partial file already on disk is RESUMED by a second
    /// `FsSink`, completes, and verifies its digest — never re-reading the path to hash,
    /// yet the streaming digest still covers the whole (prefix + new) file.
    #[test]
    fn fs_sink_resumes_partial_file_and_verifies() {
        let scratch = Scratch::new("resume");
        let content = bytes(42, 900_000);
        let path = scratch.path("resume.dat");
        let quarantine = scratch.dir.clone();

        // Pre-write a partial prefix to disk (simulating a dropped first leg).
        let prefix_len = 300_000usize;
        std::fs::write(&path, &content[..prefix_len]).expect("seed partial");

        // Open the resuming sink directly: existing_len must report the prefix length.
        {
            let resume_sink = FsSink::open(&path).expect("open resume");
            assert_eq!(
                resume_sink.existing_len(),
                prefix_len as u64,
                "FsSink resumes from the on-disk length"
            );
        }

        // Now run a full transfer leg whose sink resumes that prefix.
        let mut sender = Sender::new_with_hashes(
            0xC6,
            vec![(
                "resume.dat".into(),
                MemSource::new(content.clone()),
                Some(sha256(&content)),
            )],
        )
        .expect("s");
        let make = move |_i: usize, p: &Path| FsSink::open(p);
        let config = ReceiverConfig::new(quarantine);
        let (mut receiver, out) =
            Receiver::accept_offer(&sender.offer(), config, make).expect("offer");
        let accept = expect_accept(out);
        assert_eq!(
            accept.resume,
            vec![mouser_protocol::ResumePoint {
                file_index: 0,
                offset: prefix_len as u64
            }],
            "the receiver offers the on-disk prefix as the resume point"
        );
        sender.on_accept(&accept).expect("accept");
        drive_until_complete(&mut sender, &mut receiver).expect("resume transfer");

        assert!(
            receiver.is_complete(),
            "the resumed transfer completes + verifies"
        );
        let on_disk = std::fs::read(&path).expect("read resume.dat");
        assert_eq!(on_disk, content, "prefix + resumed bytes equal the source");
    }

    /// `FsSink::open` must refuse a path whose final component is a pre-existing symlink
    /// (the on-disk half of §7.8's "no symlink follow"): it must NOT write through the link.
    #[test]
    fn fs_sink_refuses_pre_existing_symlink() {
        use std::os::unix::fs::symlink;
        let scratch = Scratch::new("symlink");
        let target = scratch.path("secret.txt");
        std::fs::write(&target, b"do not overwrite me").expect("seed target");
        let link = scratch.path("evil.bin");
        symlink(&target, &link).expect("create symlink");

        let err = match FsSink::open(&link) {
            Ok(_) => panic!("opening a symlink must fail"),
            Err(e) => e,
        };
        assert!(
            err.to_string().contains("symlink"),
            "error should name the symlink refusal, got: {err}"
        );
        // The target was never written through the link.
        assert_eq!(
            std::fs::read(&target).expect("target intact"),
            b"do not overwrite me",
            "the symlink target must be untouched"
        );
    }

    /// A non-contiguous positioned write (a gap) is a hard error, never silent corruption.
    #[test]
    fn fs_sink_rejects_non_contiguous_write() {
        let scratch = Scratch::new("gap");
        let path = scratch.path("gap.bin");
        let mut sink = FsSink::open(&path).expect("open");
        sink.write_at(0, b"hello").expect("first write");
        // Writing at 10 while only 5 bytes are held is a gap → SinkError.
        assert!(
            sink.write_at(10, b"world").is_err(),
            "a gap must be rejected"
        );
        // Re-writing an already-committed offset is also rejected.
        assert!(
            sink.write_at(0, b"x").is_err(),
            "a rewrite must be rejected"
        );
    }
}
