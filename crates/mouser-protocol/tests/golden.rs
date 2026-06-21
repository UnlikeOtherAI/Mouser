//! Golden vectors and forward-compat conformance for the wire protocol.
//! These are the canonical byte expectations referenced by docs/communication-interface.md §0.1.

use mouser_protocol::{
    decode_datagram, decode_frame, encode_frame, encode_motion, encode_motion_rel, from_cbor,
    to_cbor, AckStatus, BlockedReason, CapState, Capability, CapabilitySet, ClipFormat, Datagram,
    FocusKind, GoodbyeReason, NotifyKind, Os, Ping, PointerMode, PointerMotion, PointerMotionRel,
    Role, ScrollUnit, TransferReason, TAG_POINTER_MOTION, TAG_POINTER_MOTION_REL, TYPE_PING,
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
