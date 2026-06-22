//! Mouser wire protocol — control-stream framing, the message envelope, and the
//! wire enums. This crate is the byte-level contract from
//! `docs/communication-interface.md` (v2.5); two independently-built engines that
//! agree on this crate can interoperate.
//!
//! The decode path is held to a no-panic discipline (the workspace clippy lints
//! deny `unwrap_used`/`panic`/`indexing_slicing`) so attacker-controlled bytes
//! cannot crash an engine.

pub mod codec;
pub mod datagram;
pub mod enums;
pub mod framing;
pub mod messages;

pub use codec::{from_cbor, to_cbor, CodecError};
pub use datagram::{
    decode_datagram, encode_motion, encode_motion_rel, Datagram, DatagramError, PointerMotion,
    PointerMotionRel, TAG_POINTER_MOTION, TAG_POINTER_MOTION_REL,
};
pub use enums::{
    AckStatus, BlockedReason, CapState, Capability, CapabilitySet, ClipFormat, FocusKind,
    GoodbyeReason, NotifyKind, Os, PointerMode, Role, ScrollUnit, TransferReason,
};
pub use framing::{
    decode_frame, encode_bulk_frame, encode_frame, encode_frame_capped, Frame, FrameError,
    MAX_BULK_FRAME, MAX_CONTROL_FRAME,
};
pub use messages::{
    BulkHello, CapabilityState, ClipboardData, ClipboardEntry, ClipboardOffer, ClipboardPull,
    FileAccept, FileAck, FileChunk, FileDone, FileEntry, FileOffer, FileReject, FocusState,
    Goodbye, Heartbeat, Hello, HelloAck, KeyEvent, OwnershipAck, OwnershipRequest,
    OwnershipTransfer, PairingResult, Ping, PointerButton, PointerModeReq, Pong, ResumePoint,
    Scroll, TYPE_BULK_HELLO, TYPE_CAPABILITY_STATE, TYPE_CLIPBOARD_DATA, TYPE_CLIPBOARD_OFFER,
    TYPE_CLIPBOARD_PULL, TYPE_DEVICE_NAME, TYPE_FILE_ACCEPT, TYPE_FILE_ACK, TYPE_FILE_CHUNK,
    TYPE_FILE_DONE, TYPE_FILE_OFFER, TYPE_FILE_REJECT, TYPE_FOCUS_STATE, TYPE_GOODBYE,
    TYPE_HEARTBEAT, TYPE_HELLO, TYPE_HELLO_ACK, TYPE_KEY_EVENT, TYPE_OWNERSHIP_ACK,
    TYPE_OWNERSHIP_REQUEST, TYPE_OWNERSHIP_TRANSFER, TYPE_PAIRING_RESULT, TYPE_PING,
    TYPE_POINTER_BUTTON, TYPE_POINTER_MODE_REQ, TYPE_PONG, TYPE_SCROLL,
};
