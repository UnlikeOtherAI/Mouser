//! Mouser wire protocol — control-stream framing, the message envelope, and the
//! wire enums. This crate is the byte-level contract from
//! `docs/communication-interface.md` (v2.3); two independently-built engines that
//! agree on this crate can interoperate.
//!
//! The decode path is held to a no-panic discipline (see the crate-level lint
//! denies below) so attacker-controlled bytes cannot crash an engine.

#![deny(clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]

pub mod codec;
pub mod enums;
pub mod framing;
pub mod messages;

pub use codec::{from_cbor, to_cbor, CodecError};
pub use enums::{
    AckStatus, BlockedReason, CapState, Capability, CapabilitySet, ClipFormat, FocusKind,
    GoodbyeReason, NotifyKind, Os, PointerMode, Role, ScrollUnit, TransferReason,
};
pub use framing::{decode_frame, encode_frame, Frame, FrameError, MAX_CONTROL_FRAME};
pub use messages::{Ping, TYPE_HELLO_ACK, TYPE_PING};
