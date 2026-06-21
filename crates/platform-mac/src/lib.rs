//! `platform-mac` — macOS input capture/injection **spike** for Mouser.
//!
//! ## Status: de-risking spike
//! This crate is a standalone spike. It deliberately does **not** depend on
//! `mouser-core`'s `InputCapture` / `InputInjection` traits yet — the goal is to
//! prove the Core Graphics injection/capture path works on real macOS before
//! committing to a trait shape. Reconciliation with the core traits is a later,
//! separate step.
//!
//! It also deliberately does **not** set `[lints] workspace = true`: the
//! workspace forbids `unsafe_code`, but the macOS path is C-API-driven
//! (`CGEvent*`, `CGEventTap*`, `CGWarpMouseCursorPosition`). The `unsafe` lives
//! inside the `core-graphics` crate's safe wrappers; this crate calls those
//! wrappers and adds no `unsafe` of its own today, but it does not adopt the
//! workspace `forbid(unsafe_code)` so that constraint isn't a future trap.
//!
//! ## Capability reality (see `docs/tech-stack.md` §4)
//! - **Injection** (`inject`): needs **Accessibility** for posted events to take
//!   effect; the `CGWarpMouseCursorPosition` path moves the cursor without any
//!   grant. No grant → posted events are silently dropped.
//! - **Capture** (`capture`): needs **Accessibility + Input Monitoring**;
//!   **Secure Event Input** can suppress key capture; lock screen = local only.
//!   A full capture test can be blocked without TCC grants — see `capture`.
//!
//! All coordinates are global display points, top-left origin, y-down, matching
//! the wire protocol's absolute `PointerMotion` space (§7.6).

#![cfg(target_os = "macos")]

pub mod capture;
pub mod display_info;
pub mod inject;
pub mod keymap;

pub use capture::{install_listen_only_tap, CaptureError};
pub use display_info::{main_display_bounds, DisplayBounds};
pub use inject::{cursor_position, key_press, left_click, move_cursor, scroll, InjectError};
pub use keymap::hid_usage_to_cgkeycode;
