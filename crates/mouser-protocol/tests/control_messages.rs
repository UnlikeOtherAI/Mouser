//! Round-trip, determinism, and golden-vector conformance for the §7.1/§7.4/§7.5
//! control messages (live input + ownership). These lock the CBOR field order and
//! the framed envelope so two independently-built engines interoperate.

use mouser_protocol::{
    decode_frame, encode_frame, from_cbor, to_cbor, BlockedReason, CapState, Capability,
    CapabilitySet, CapabilityState, FocusKind, FocusState, Goodbye, GoodbyeReason, Heartbeat,
    Hello, KeyEvent, Os, OwnershipAck, OwnershipRequest, OwnershipTransfer, PairingResult,
    PointerButton, PointerMode, PointerModeReq, Pong, Role, Scroll, ScrollUnit, TransferReason,
    TYPE_KEY_EVENT, TYPE_PAIRING_RESULT, TYPE_PONG,
};
use std::collections::BTreeSet;

/// Encode → decode equals original, and re-encoding the decoded value is byte-identical
/// (canonical/deterministic). This is the core interop guarantee for every message.
fn assert_canonical<T>(value: &T)
where
    T: serde::Serialize + serde::de::DeserializeOwned + PartialEq + std::fmt::Debug,
{
    let bytes = to_cbor(value).expect("encode");
    let back: T = from_cbor(&bytes).expect("decode");
    assert_eq!(&back, value, "round-trip must preserve the value");
    let reencoded = to_cbor(&back).expect("re-encode");
    assert_eq!(bytes, reencoded, "encoding must be deterministic/canonical");
}

#[test]
fn session_and_liveness_messages_roundtrip() {
    assert_canonical(&Hello {
        device_id: vec![0xAB; 32],
        name: "Studio".to_string(),
        os: Os::Macos,
        engine_version: "0.1.0".to_string(),
        capabilities: CapabilitySet(BTreeSet::from([Capability::Keyboard, Capability::Mouse])),
        role: Role::Eligible,
        session_id: 0xDEAD_BEEF,
        channel_sig: vec![0x11; 64],
    });
    assert_canonical(&PairingResult {
        accepted: true,
        reason: None,
    });
    assert_canonical(&PairingResult {
        accepted: false,
        reason: Some("SAS mismatch".to_string()),
    });
    assert_canonical(&Pong { id: 7 });
    assert_canonical(&Heartbeat { seq: 42 });
    assert_canonical(&Goodbye {
        reason: GoodbyeReason::Sleep,
    });
}

#[test]
fn ownership_messages_roundtrip() {
    assert_canonical(&OwnershipTransfer {
        to: vec![0x01; 32],
        owner_epoch: 5,
        layout_rev: 3,
        reason: TransferReason::EdgeCross,
    });
    assert_canonical(&OwnershipAck {
        owner_epoch: 5,
        accepted: true,
        reason: None,
    });
    assert_canonical(&OwnershipAck {
        owner_epoch: 5,
        accepted: false,
        reason: Some("blocked".to_string()),
    });
    assert_canonical(&FocusState {
        owner: vec![0x02; 32],
        owner_epoch: 6,
        state: FocusKind::Active,
    });
    assert_canonical(&CapabilityState {
        device_id: vec![0x03; 32],
        capture: CapState::Available,
        inject: CapState::PermissionMissing,
        reason: BlockedReason::Permission,
    });
    assert_canonical(&OwnershipRequest {
        from: vec![0x04; 32],
        reason: TransferReason::UiSelect,
    });
    assert_canonical(&PointerModeReq {
        owner_epoch: 7,
        mode: PointerMode::Relative,
    });
}

#[test]
fn input_messages_roundtrip() {
    assert_canonical(&KeyEvent {
        usage: 0x04,
        down: true,
        mods: 0,
        owner_epoch: 1,
        ctr: 2,
    });
    assert_canonical(&PointerButton {
        button: 1,
        down: false,
        owner_epoch: 1,
        ctr: 9,
    });
    assert_canonical(&Scroll {
        dx: -3,
        dy: 120,
        unit: ScrollUnit::Detent120,
        owner_epoch: 1,
        ctr: 10,
    });
}

#[test]
fn pong_golden_vector() {
    // Mirrors the §0.1 Ping example but on type 0x06: {"id":7} = A1 62 69 64 07.
    let payload = to_cbor(&Pong { id: 7 }).expect("encode");
    assert_eq!(payload, [0xA1, 0x62, 0x69, 0x64, 0x07]);
    let frame = encode_frame(TYPE_PONG, 0, &payload).expect("frame");
    assert_eq!(
        frame,
        [0x09, 0x00, 0x00, 0x00, 0x06, 0x00, 0x00, 0x00, 0xA1, 0x62, 0x69, 0x64, 0x07],
    );
}

#[test]
fn pairing_result_omits_none_reason() {
    // `reason: None` must drop the key entirely (skip_serializing_if): a 1-entry map
    // {"accepted": true} = A1 68 "accepted" F5.
    let bytes = to_cbor(&PairingResult {
        accepted: true,
        reason: None,
    })
    .expect("encode");
    assert_eq!(
        bytes,
        [0xA1, 0x68, 0x61, 0x63, 0x63, 0x65, 0x70, 0x74, 0x65, 0x64, 0xF5],
        "None reason must be omitted, not encoded as null"
    );
    // Present reason adds a second key, so the map header becomes A2.
    let with_reason = to_cbor(&PairingResult {
        accepted: true,
        reason: Some("x".to_string()),
    })
    .expect("encode");
    assert_eq!(
        with_reason.first(),
        Some(&0xA2),
        "two keys when reason present"
    );
}

#[test]
fn key_event_golden_vector_field_order() {
    // Locks the §7.5 field order as CBOR map keys: usage, down, mods, owner_epoch, ctr.
    let payload = to_cbor(&KeyEvent {
        usage: 4,
        down: true,
        mods: 0,
        owner_epoch: 1,
        ctr: 2,
    })
    .expect("enc");
    assert_eq!(
        payload,
        [
            0xA5, // map(5)
            0x65, 0x75, 0x73, 0x61, 0x67, 0x65, 0x04, // "usage": 4
            0x64, 0x64, 0x6F, 0x77, 0x6E, 0xF5, // "down": true
            0x64, 0x6D, 0x6F, 0x64, 0x73, 0x00, // "mods": 0
            0x6B, 0x6F, 0x77, 0x6E, 0x65, 0x72, 0x5F, 0x65, 0x70, 0x6F, 0x63, 0x68,
            0x01, // "owner_epoch": 1
            0x63, 0x63, 0x74, 0x72, 0x02, // "ctr": 2
        ],
    );
    // And it survives a full frame round-trip on type 0x40.
    let frame = encode_frame(TYPE_KEY_EVENT, 0, &payload).expect("frame");
    let (decoded, consumed) = decode_frame(&frame).expect("deframe");
    assert_eq!(consumed, frame.len());
    assert_eq!(decoded.msg_type, TYPE_KEY_EVENT);
    let round: KeyEvent = from_cbor(decoded.payload).expect("decode");
    assert_eq!(
        round,
        KeyEvent {
            usage: 4,
            down: true,
            mods: 0,
            owner_epoch: 1,
            ctr: 2
        }
    );
}

#[test]
fn pairing_result_type_is_three() {
    assert_eq!(TYPE_PAIRING_RESULT, 0x03);
}
