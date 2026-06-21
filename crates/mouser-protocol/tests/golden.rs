//! Golden vectors and forward-compat conformance for the wire protocol.
//! These are the canonical byte expectations referenced by docs/communication-interface.md §0.1.

use mouser_protocol::{
    decode_frame, encode_frame, from_cbor, to_cbor, AckStatus, Capability, CapabilitySet,
    FileAccept, FileAck, FileChunk, FileDone, FileEntry, FileOffer, FileReject, Ping, ResumePoint,
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
