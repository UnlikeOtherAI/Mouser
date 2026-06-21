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
//! workspace forbids `unsafe_code`, but the macOS path is C/Objective-C-API-driven.
//! The input spike ([`inject`]/[`capture`]) calls `core-graphics`' safe wrappers
//! (`CGEvent*`, `CGEventTap*`, `CGWarpMouseCursorPosition`) and adds no `unsafe`.
//! The **drag-and-drop spike** ([`dragdrop`]) does use `unsafe`: defining an
//! Objective-C `NSDraggingSource` class (`objc2::define_class!`), `msg_send!`, and
//! a handful of AppKit calls that `objc2` marks `unsafe`. That `unsafe` is confined
//! to [`dragdrop`]; the rest of the crate stays wrapper-only.
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
pub mod dragdrop;
pub mod inject;
pub mod keymap;

pub use capture::{install_listen_only_tap, CaptureError};
pub use display_info::{main_display_bounds, DisplayBounds};
pub use dragdrop::{
    begin_file_drag, read_dragged_file_urls, read_dragged_file_urls_from, write_file_urls,
    DragError, DragSession,
};
pub use inject::{cursor_position, key_press, left_click, move_cursor, scroll, InjectError};
pub use keymap::hid_usage_to_cgkeycode;
