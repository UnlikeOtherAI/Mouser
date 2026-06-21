//! Golden vectors and forward-compat conformance for the wire protocol.
//! These are the canonical byte expectations referenced by docs/communication-interface.md §0.1.

use mouser_protocol::{
    decode_datagram, decode_frame, encode_frame, encode_motion, encode_motion_rel, from_cbor,
    to_cbor, AckStatus, BlockedReason, CapState, Capability, CapabilitySet, ClipFormat,
    ClipboardData, ClipboardEntry, ClipboardOffer, ClipboardPull, Datagram, FileAccept, FileAck,
    FileChunk, FileDone, FileEntry, FileOffer, FileReject, FocusKind, GoodbyeReason, HelloAck,
    NotifyKind, Os, Ping, PointerMode, PointerMotion, PointerMotionRel, ResumePoint, Role,
    ScrollUnit, TransferReason, TAG_POINTER_MOTION, TAG_POINTER_MOTION_REL, TYPE_CLIPBOARD_DATA,
    TYPE_FILE_CHUNK, TYPE_PING,
};
use std::collections::BTreeSet;

#[test]
fn ping_golden_vector_matches_spec() {
    // Spec §0.1 worked example: Ping{ id: 7 }, type 0x05, frames as
    // 09 00 00 00 | 05 00 | 00 00 | A1 62 69 64 07
    let payload = to_cbor(&Ping { id: 7 }).expect("encode");
    assert_eq!(
        payload,
        [0xA1, 0x62, 0x69, 0x64, 0x07],
        "CBOR map {{\"id\":7}}"
    );

    let frame = encode_frame(TYPE_PING, 0, &payload).expect("frame");
    assert_eq!(
        frame,
        [0x09, 0x00, 0x00, 0x00, 0x05, 0x00, 0x00, 0x00, 0xA1, 0x62, 0x69, 0x64, 0x07],
    );

    let (decoded, consumed) = decode_frame(&frame).expect("deframe");
    assert_eq!(consumed, frame.len());
    assert_eq!(decoded.msg_type, TYPE_PING);
    assert_eq!(decoded.flags, 0);
    let round: Ping = from_cbor(decoded.payload).expect("decode");
    assert_eq!(round, Ping { id: 7 });
}

#[test]
fn enum_roundtrips_and_unknown_maps_not_errors() {
    // Known value round-trips through its integer discriminant.
    let enc = to_cbor(&AckStatus::Pending).expect("encode");
    assert_eq!(enc, [0x02], "AckStatus::Pending encodes as CBOR uint 2");
    let back: AckStatus = from_cbor(&enc).expect("decode");
    assert_eq!(back, AckStatus::Pending);

    // CBOR uint 99 (0x18 0x63) is an unknown discriminant: it MUST decode to Unknown,
    // not error — this is the §2 forward-compatibility guarantee.
    let unknown: AckStatus = from_cbor(&[0x18, 0x63]).expect("forward-compat decode");
    assert_eq!(unknown, AckStatus::Unknown);
}

#[test]
fn unknown_frame_type_is_skippable() {
    // An unknown message type must still be skippable via its length prefix.
    let body = [1u8, 2, 3];
    let frame = encode_frame(0xFFFE, 0, &body).expect("frame");
    let (f, consumed) = decode_frame(&frame).expect("deframe");
    assert_eq!(f.msg_type, 0xFFFE);
    assert_eq!(f.payload, &body);
    assert_eq!(consumed, frame.len());
}

#[test]
fn capability_set_is_ascending_and_drops_unknown() {
    // Encodes as an ascending CBOR integer array: {Mouse=1, Keyboard=0} -> [0,1].
    let set = CapabilitySet(BTreeSet::from([Capability::Mouse, Capability::Keyboard]));
    assert_eq!(to_cbor(&set).expect("encode"), [0x82, 0x00, 0x01]);

    // Decoding an array with an unknown member (99) drops it, keeping {0,1,2}.
    // CBOR array(4)[2, 0, 99, 1] = 84 02 00 18 63 01
    let decoded: CapabilitySet =
        from_cbor(&[0x84, 0x02, 0x00, 0x18, 0x63, 0x01]).expect("forward-compat decode");
    assert_eq!(
        decoded,
        CapabilitySet(BTreeSet::from([
            Capability::Keyboard,
            Capability::Mouse,
            Capability::Clipboard,
        ]))
    );
}

#[test]
fn pointer_motion_datagram_golden_vector() {
    // Byte-exact golden (H9): tag 0x01 + postcard varint body.
    // owner_epoch=1 (u64 varint 0x01), seq=2 (u32 varint 0x02),
    // display_id=3 (u32 varint 0x03), x=-4 (i32 zig-zag -> 7 -> 0x07),
    // y=5 (i32 zig-zag -> 10 -> 0x0A).
    let m = PointerMotion {
        owner_epoch: 1,
        seq: 2,
        display_id: 3,
        x: -4,
        y: 5,
    };
    let bytes = encode_motion(&m).expect("encode");
    assert_eq!(
        bytes,
        [TAG_POINTER_MOTION, 0x01, 0x02, 0x03, 0x07, 0x0A],
        "PointerMotion datagram golden bytes"
    );
    assert_eq!(decode_datagram(&bytes).expect("decode"), Datagram::Motion(m));
}

#[test]
fn pointer_motion_rel_datagram_golden_vector() {
    // Byte-exact golden (H9): tag 0x02 + postcard varint body.
    // owner_epoch=1 (0x01), seq=2 (0x02), dx_acc=-3 (i64 zig-zag -> 5 -> 0x05),
    // dy_acc=4 (i64 zig-zag -> 8 -> 0x08).
    let m = PointerMotionRel {
        owner_epoch: 1,
        seq: 2,
        dx_acc: -3,
        dy_acc: 4,
    };
    let bytes = encode_motion_rel(&m).expect("encode");
    assert_eq!(
        bytes,
        [TAG_POINTER_MOTION_REL, 0x01, 0x02, 0x05, 0x08],
        "PointerMotionRel datagram golden bytes"
    );
    assert_eq!(
        decode_datagram(&bytes).expect("decode"),
        Datagram::MotionRel(m)
    );
}

#[test]
fn every_appendix_c_enum_known_discriminant_encodes_to_exact_cbor_byte() {
    // Table (H9): each known Appendix-C enum discriminant is < 24, so its canonical
    // CBOR encoding is a single uint byte equal to the discriminant. We list every
    // known variant per enum and assert byte-exact encode + lossless decode.
    macro_rules! cases {
        ($($variant:expr => $byte:expr),+ $(,)?) => {{
            $(
                let enc = to_cbor(&$variant).expect("encode");
                assert_eq!(
                    enc, [$byte],
                    concat!(stringify!($variant), " must encode to the exact CBOR byte"),
                );
                // Round-trips back to the same known variant (not Unknown).
                let back = from_cbor(&enc).expect("decode");
                assert_eq!($variant, back, concat!(stringify!($variant), " round-trip"));
            )+
        }};
    }

    cases!(
        Os::Macos => 0, Os::Windows => 1, Os::Linux => 2, Os::Ios => 3, Os::Android => 4,
        Role::Eligible => 0, Role::Ineligible => 1,
        AckStatus::Accepted => 0, AckStatus::Rejected => 1, AckStatus::Pending => 2,
        GoodbyeReason::Shutdown => 0, GoodbyeReason::Sleep => 1,
        GoodbyeReason::UserQuit => 2, GoodbyeReason::NetworkLeave => 3,
        TransferReason::EdgeCross => 0, TransferReason::Hotkey => 1,
        TransferReason::UiSelect => 2, TransferReason::LocalReclaim => 3,
        FocusKind::Active => 0, FocusKind::Standby => 1,
        FocusKind::Disconnected => 2, FocusKind::InputBlocked => 3,
        CapState::Available => 0, CapState::PermissionMissing => 1,
        CapState::SecureContext => 2, CapState::Unsupported => 3,
        BlockedReason::None => 0, BlockedReason::SecureDesktop => 1,
        BlockedReason::LockScreen => 2, BlockedReason::SecureInputField => 3,
        BlockedReason::Permission => 4, BlockedReason::CompositorUnsupported => 5,
        ClipFormat::Utf8Text => 0, ClipFormat::Html => 1, ClipFormat::Png => 2,
        ClipFormat::UriList => 3, ClipFormat::Rtf => 4,
        ScrollUnit::Detent120 => 0, ScrollUnit::LogicalPixel => 1,
        PointerMode::Absolute => 0, PointerMode::Relative => 1,
        NotifyKind::DeviceConnected => 0, NotifyKind::DeviceDisconnected => 1,
        NotifyKind::ConfigChanged => 2, NotifyKind::CoordinatorChanged => 3,
    );

    // `Capability` has no Unknown sentinel; its members encode as the raw uint too.
    for (cap, byte) in [
        (Capability::Keyboard, 0u8),
        (Capability::Mouse, 1),
        (Capability::Clipboard, 2),
        (Capability::FileTransfer, 3),
        (Capability::Webcam, 4),
        (Capability::Audio, 5),
        (Capability::CoordinatorEligible, 6),
        (Capability::RemoteControlOnly, 7),
    ] {
        assert_eq!(to_cbor(&u16::from(cap)).expect("encode"), [byte]);
    }
}

#[test]
fn struct_decode_ignores_unknown_field_and_preserves_known() {
    // Struct-level forward-compat (H9, §2): a CBOR map carrying an extra unknown key
    // must decode, ignoring the unknown field while preserving the known one.
    // Map(2){ "id": 7, "future": 1 } =
    //   A2 62 69 64 07 66 66 75 74 75 72 65 01
    let bytes = [
        0xA2, // map(2)
        0x62, 0x69, 0x64, // "id"
        0x07, // 7
        0x66, 0x66, 0x75, 0x74, 0x75, 0x72, 0x65, // "future"
        0x01, // 1
    ];
    let decoded: Ping = from_cbor(&bytes).expect("forward-compat struct decode");
    assert_eq!(decoded, Ping { id: 7 });
}

#[test]
fn file_transfer_messages_round_trip() {
    // §7.8: every file-transfer message survives a CBOR encode/decode round-trip.
    let offer = FileOffer {
        transfer_id: 0x1122334455667788,
        files: vec![
            FileEntry {
                name: "report.pdf".into(),
                size: 9_000_000,
            },
            FileEntry {
                name: "notes.txt".into(),
                size: 17,
            },
        ],
    };
    let back: FileOffer = from_cbor(&to_cbor(&offer).expect("enc")).expect("dec");
    assert_eq!(back, offer);

    let accept = FileAccept {
        transfer_id: 7,
        resume: vec![ResumePoint {
            file_index: 1,
            offset: 4096,
        }],
    };
    let back: FileAccept = from_cbor(&to_cbor(&accept).expect("enc")).expect("dec");
    assert_eq!(back, accept);

    let reject = FileReject {
        transfer_id: 7,
        reason: "permission denied".into(),
    };
    let back: FileReject = from_cbor(&to_cbor(&reject).expect("enc")).expect("dec");
    assert_eq!(back, reject);

    let chunk = FileChunk {
        transfer_id: 7,
        file_index: 0,
        offset: 1 << 20,
        data: vec![1, 2, 3, 4, 5],
    };
    let back: FileChunk = from_cbor(&to_cbor(&chunk).expect("enc")).expect("dec");
    assert_eq!(back, chunk);

    let ack = FileAck {
        transfer_id: 7,
        file_index: 0,
        acked_through: 8 * 1024 * 1024,
    };
    let back: FileAck = from_cbor(&to_cbor(&ack).expect("enc")).expect("dec");
    assert_eq!(back, ack);

    let done = FileDone {
        transfer_id: 7,
        ok: true,
    };
    let back: FileDone = from_cbor(&to_cbor(&done).expect("enc")).expect("dec");
    assert_eq!(back, done);
}

#[test]
fn file_chunk_golden_vector_encodes_data_as_byte_string() {
    // Golden bytes for FileChunk{transfer_id:1, file_index:0, offset:0, data:[DEADBEEF]}.
    // §0.1: structs are definite-length CBOR maps keyed by the field-name string, and a
    // `bytes` field is a CBOR **byte string** — NOT an array of integers. The decisive
    // bytes are the `data` value `0x44 DE AD BE EF` (0x44 = major type 2 / byte-string of
    // length 4), which would be `0x84 ...` if it (wrongly) encoded as an array.
    let chunk = FileChunk {
        transfer_id: 1,
        file_index: 0,
        offset: 0,
        data: vec![0xDE, 0xAD, 0xBE, 0xEF],
    };
    let encoded = to_cbor(&chunk).expect("encode");
    assert_eq!(
        encoded,
        [
            0xA4, // map(4)
            0x6B, 0x74, 0x72, 0x61, 0x6E, 0x73, 0x66, 0x65, 0x72, 0x5F, 0x69, 0x64, // "transfer_id"
            0x01, // 1
            0x6A, 0x66, 0x69, 0x6C, 0x65, 0x5F, 0x69, 0x6E, 0x64, 0x65, 0x78, // "file_index"
            0x00, // 0
            0x66, 0x6F, 0x66, 0x66, 0x73, 0x65, 0x74, // "offset"
            0x00, // 0
            0x64, 0x64, 0x61, 0x74, 0x61, // "data"
            0x44, 0xDE, 0xAD, 0xBE, 0xEF, // byte-string(4) DE AD BE EF
        ],
        "FileChunk golden bytes (§0.1 byte-string encoding for `data`)"
    );

    // And it must frame + deframe through the §0.2 envelope on the bulk stream.
    let frame = encode_frame(TYPE_FILE_CHUNK, 0, &encoded).expect("frame");
    let (decoded, consumed) = decode_frame(&frame).expect("deframe");
    assert_eq!(consumed, frame.len());
    assert_eq!(decoded.msg_type, TYPE_FILE_CHUNK);
    let round: FileChunk = from_cbor(decoded.payload).expect("decode");
    assert_eq!(round, chunk);
}

#[test]
fn clipboard_messages_round_trip() {
    // §7.7: Offer/Pull/Data all survive a CBOR encode/decode round-trip.
    let offer = ClipboardOffer {
        entries: vec![
            ClipboardEntry {
                format: ClipFormat::Utf8Text,
                hash: vec![0xAB; 32],
                size: 11,
            },
            ClipboardEntry {
                format: ClipFormat::Png,
                hash: vec![0xCD; 32],
                size: 4_000_000,
            },
        ],
        origin: vec![0x01; 32],
    };
    let back: ClipboardOffer = from_cbor(&to_cbor(&offer).expect("enc")).expect("dec");
    assert_eq!(back, offer);

    let pull = ClipboardPull {
        hash: vec![0xCD; 32],
        format: ClipFormat::Png,
    };
    let back: ClipboardPull = from_cbor(&to_cbor(&pull).expect("enc")).expect("dec");
    assert_eq!(back, pull);

    let data = ClipboardData {
        hash: vec![0xCD; 32],
        format: ClipFormat::Utf8Text,
        offset: 0,
        data: b"hello world".to_vec(),
        last: true,
    };
    let back: ClipboardData = from_cbor(&to_cbor(&data).expect("enc")).expect("dec");
    assert_eq!(back, data);
}

#[test]
fn clipboard_data_encodes_payload_as_byte_string() {
    // §0.1: `data` (and `hash`) are CBOR **byte strings**, not arrays of ints. The
    // decisive bytes are `data` = `0x44 DE AD BE EF` (0x44 = byte-string of length 4).
    let data = ClipboardData {
        hash: vec![0x00; 4],
        format: ClipFormat::Png,
        offset: 0,
        data: vec![0xDE, 0xAD, 0xBE, 0xEF],
        last: true,
    };
    let encoded = to_cbor(&data).expect("encode");
    // The 4-byte payload must appear as `0x44 DE AD BE EF` somewhere in the encoding.
    let needle = [0x44, 0xDE, 0xAD, 0xBE, 0xEF];
    assert!(
        encoded.windows(needle.len()).any(|w| w == needle),
        "ClipboardData.data must encode as a CBOR byte string (§0.1)"
    );
    // And it frames + deframes through the §0.2 envelope.
    let frame = encode_frame(TYPE_CLIPBOARD_DATA, 0, &encoded).expect("frame");
    let (decoded, consumed) = decode_frame(&frame).expect("deframe");
    assert_eq!(consumed, frame.len());
    assert_eq!(decoded.msg_type, TYPE_CLIPBOARD_DATA);
    let round: ClipboardData = from_cbor(decoded.payload).expect("decode");
    assert_eq!(round, data);
}

#[test]
fn hello_ack_round_trips_with_and_without_reason() {
    // §7.1: `reason` is optional and omitted when None.
    let accepted = HelloAck {
        status: AckStatus::Accepted,
        reason: None,
    };
    let back: HelloAck = from_cbor(&to_cbor(&accepted).expect("enc")).expect("dec");
    assert_eq!(back, accepted);

    let rejected = HelloAck {
        status: AckStatus::Rejected,
        reason: Some("untrusted device".into()),
    };
    let back: HelloAck = from_cbor(&to_cbor(&rejected).expect("enc")).expect("dec");
    assert_eq!(back, rejected);
}

#[test]
fn capability_set_drops_unknown_and_out_of_range_members() {
    // §0.1/§2 forward-compat: an unrecognized, out-of-`u16`, or negative member is
    // **dropped**, never an error. Build the CBOR array [0, 1, 2, 99, 65536, -1] and
    // assert it decodes to exactly {Keyboard, Mouse, Clipboard}.
    let raw: Vec<i128> = vec![0, 1, 2, 99, 65536, -1];
    let bytes = to_cbor(&raw).expect("encode array");
    let set: CapabilitySet = from_cbor(&bytes).expect("forward-compat set decode");
    let expected: BTreeSet<Capability> =
        [Capability::Keyboard, Capability::Mouse, Capability::Clipboard]
            .into_iter()
            .collect();
    assert_eq!(set.0, expected);
}

#[test]
fn capability_set_rejects_non_integer_member() {
    // A non-integer member is malformed (§0.1) — decode errors (distinct from an unknown
    // integer, which is dropped above). Array ["x"] = 0x81 0x61 0x78.
    let bytes = [0x81, 0x61, 0x78];
    let r: Result<CapabilitySet, _> = from_cbor(&bytes);
    assert!(r.is_err(), "non-integer capability member must error");
}
