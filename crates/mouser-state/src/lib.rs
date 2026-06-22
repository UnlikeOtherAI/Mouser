//! mouser-state — the §7.2 replicated cluster-state CRDT.
//!
//! A thin, typed wrapper around an [`automerge`] document holding the **shared,
//! non-security** cluster config (spec Appendix A): the `devices` map, the
//! per-device monitor `layout` with a monotonic `layout_rev`, the `aliases`
//! map, and cluster-wide `input_prefs`. Permissions and the trusted list are
//! deliberately **not** replicated here — they are a local, non-replicated
//! policy store (spec §7.2 / §9).
//!
//! The CRDT format is pinned to `fmt = 1` ([`STATE_FMT`]) and automerge is the
//! pinned engine. The wire ops in spec §7.2 map onto this API as:
//!
//! | wire op `[NN]`     | method                                   |
//! |--------------------|------------------------------------------|
//! | `StateDelta`/`StateChanges` | [`SharedState::apply_changes`]  |
//! | `StateRequest.have_heads`   | [`SharedState::heads`]          |
//! | reply to `StateRequest`     | [`SharedState::changes_since`]  |
//! | `StateSnapshot.full_state`  | [`SharedState::snapshot`] / [`SharedState::load`] |
//!
//! Runtime paths are panic-free: the workspace clippy lints deny
//! `unwrap_used`/`panic`/`indexing_slicing`, and `unsafe` is forbidden.

mod error;
mod model;
mod state;

pub use error::{StateError, StateResult};
pub use model::{DeviceInfo, InputPrefs, Monitor};
pub use state::{device_id_hex, SharedState, STATE_FMT};

/// A device's permanent identifier: `SHA-256(SubjectPublicKeyInfo)` of its
/// Ed25519 identity key (spec §3). 32 bytes; in the CRDT it is keyed as
/// lowercase hex (`device_id_hex`).
pub type DeviceId = [u8; 32];
