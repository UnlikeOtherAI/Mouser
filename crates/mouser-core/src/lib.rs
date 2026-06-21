//! mouser-core — platform-agnostic engine logic with **no I/O**.
//!
//! This crate holds the pure, deterministic pieces of a Mouser cluster member so
//! they can be unit-tested in isolation and reused unchanged across every platform
//! adapter and the mobile bindings:
//!
//! - [`identity`] — the permanent Ed25519 device key and the
//!   `device_id = SHA-256(SubjectPublicKeyInfo)` derivation (spec §3).
//! - [`platform`] — the trait contracts the per-OS adapters implement
//!   (input capture/injection, clipboard, tray). Definitions only.
//! - [`ownership`] — the `owner_epoch` ownership state machine (spec §7.4).
//! - [`election`] — the lease-based coordinator election state machine (spec §7.10).
//!
//! Everything here is clock-free in the sense that no module reads the OS clock:
//! time enters [`election`] only through an injected monotonic instant, so the
//! logic stays fully deterministic and testable.

pub mod election;
pub mod identity;
pub mod ownership;
pub mod platform;

/// A device's permanent identifier: `SHA-256(SubjectPublicKeyInfo)` of its Ed25519
/// identity key (spec §3). 32 bytes, compared byte-for-byte; never derived from
/// name or address.
pub type DeviceId = [u8; 32];

pub use election::{Election, ElectionEvent, Lease};
pub use identity::{
    device_id_from_public_key, device_id_from_public_key_bytes, DeviceIdentity, IdentityError,
};
pub use ownership::{Ownership, OwnershipUpdate, RejectReason};
