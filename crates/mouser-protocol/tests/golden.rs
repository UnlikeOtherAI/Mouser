//! Golden vectors and forward-compat conformance for the wire protocol.
//! These are the canonical byte expectations referenced by docs/communication-interface.md §0.1.

use mouser_protocol::{decode_frame, encode_frame, from_cbor, to_cbor, AckStatus, Ping, TYPE_PING};

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
