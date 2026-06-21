//! Mouser wire protocol — control-stream framing, the message envelope, and the
//! wire enums. This crate is the byte-level contract from
//! `docs/communication-interface.md` (v2.3); two independently-built engines that
//! agree on this crate can interoperate.
//!
//! The decode path is held to a no-panic discipline (see the crate-level lint
//! denies below) so attacker-controlled bytes cannot crash an engine.

#![deny(clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]

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
    BulkHello, FileAccept, FileAck, FileChunk, FileDone, FileEntry, FileOffer, FileReject, Ping,
    ResumePoint, TYPE_BULK_HELLO, TYPE_FILE_ACCEPT, TYPE_FILE_ACK, TYPE_FILE_CHUNK, TYPE_FILE_DONE,
    TYPE_FILE_OFFER, TYPE_FILE_REJECT, TYPE_HELLO_ACK, TYPE_PING,
};
