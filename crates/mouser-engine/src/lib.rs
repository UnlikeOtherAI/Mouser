//! mouser-engine — the runtime that turns the wire protocol + platform adapters into
//! a working KVM: capture local input, hand ownership across screen edges, forward
//! input to the owner, and inject it on the target (architecture §4.6, spec §7.4–§7.6).
//!
//! - [`core`] is a **pure, sans-IO state machine** ([`EngineCore`]): all the hard
//!   logic — edge-crossing, ownership handoff, anti-replay, heartbeat-timeout reclaim —
//!   expressed as `event → Vec<Action>`, fully unit-testable.
//! - [`runtime`] is the thin async shell ([`RuntimeHandle`]) that drives the core over
//!   a `mouser-net` connection and a `mouser-core` injection adapter.
//!
//! v1 is single-peer source→target (the `mouserd` binary plugs in the macOS adapters);
//! multi-peer/CRDT layout, the §5 SAS pairing UI, and clipboard live above this.

pub mod core;
pub mod runtime;

pub use core::{Action, CaptureDecision, Edge, EdgeLayout, EngineCore, Inject, Role};
pub use runtime::RuntimeHandle;
