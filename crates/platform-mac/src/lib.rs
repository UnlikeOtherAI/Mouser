//! `platform-mac` — the macOS input capture/injection adapter for Mouser.
//!
//! [`adapter::MacInjector`] / [`adapter::MacCapture`] implement the
//! `mouser_core::InputInjection` / `InputCapture` trait contracts (audit H2);
//! the free functions in [`inject`] / the tap helpers in [`capture`] are the
//! low-level bodies. [`keymap`] is the HID↔CGKeyCode table plus the `mods`
//! translation (audit H11); [`display_info`] enumerates all displays so motion
//! routes to the right monitor (audit M1).
//!
//! It deliberately does **not** set `[lints] workspace = true`: the workspace
//! forbids `unsafe_code`, but the macOS path is C-API-driven (`CGEvent*`,
//! `CGEventTap*`, `CGWarpMouseCursorPosition`, CoreFoundation run-loop fns).
//!
//! ## Capability reality (see `docs/tech-stack.md` §4)
//! - **Injection** (`inject`): needs **Accessibility** for posted events to take
//!   effect; the `CGWarpMouseCursorPosition` path moves the cursor without any
//!   grant. No grant → posted events are silently dropped.
//! - **Capture** (`capture`/`adapter`): needs **Accessibility + Input
//!   Monitoring**; a *suppress-capable* (default) `CGEventTap` additionally
//!   requires Accessibility, else [`adapter::MacCapture`] falls back to
//!   listen-only and reports `can_suppress() == false` (audit H3). **Secure
//!   Event Input** can withhold key capture; lock screen = local only.
//!
//! All coordinates are global display points, top-left origin, y-down, matching
//! the wire protocol's absolute `PointerMotion` space (§7.6).

#![cfg(target_os = "macos")]

pub mod adapter;
pub mod capture;
pub mod display_info;
pub mod inject;
pub mod keymap;

pub use adapter::{MacCapture, MacInjector};
pub use capture::{install_listen_only_tap, CaptureError};
pub use display_info::{
    active_display_bounds, display_bounds, main_display_bounds, DisplayBounds,
};
pub use inject::{
    button, cursor_position, key_press, left_click, move_cursor, move_cursor_rel, scroll,
    InjectError,
};
pub use keymap::{
    cgkeycode_to_hid_usage, hid_usage_to_cgkeycode, mods_to_cgflags, mods_to_cgkeycodes,
    supported_hid_usages,
};
