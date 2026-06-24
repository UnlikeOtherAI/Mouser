//! `platform-mac` — the macOS input capture/injection adapter for Mouser.
//!
//! [`adapter::MacInjector`] / [`adapter::MacCapture`] implement the
//! `mouser_core::InputInjection` / `InputCapture` trait contracts (audit H2);
//! the free functions in [`inject`] / the tap helpers in [`capture`] are the
//! low-level bodies. [`keymap`] is the HID↔CGKeyCode table plus the `mods`
//! translation (audit H11); [`display_info`] enumerates all displays so motion
//! routes to the right monitor (audit M1). [`tray::MacTray`] implements the
//! `mouser_core::Tray` contract over an `NSStatusItem` (main-thread / AppKit-host
//! only — see its module docs).
//!
//! It deliberately does **not** set `[lints] workspace = true`: the workspace
//! forbids `unsafe_code`, but the macOS path is C/Objective-C-API-driven.
//! The input spike ([`inject`]/[`capture`]) mostly calls `core-graphics`' safe
//! wrappers (`CGEvent*`, `CGEventTap*`, `CGWarpMouseCursorPosition`,
//! CoreFoundation run-loop fns); the cursor-visibility hook uses the crate's
//! raw CoreGraphics binding directly. The **drag-and-drop spike** ([`dragdrop`])
//! also uses `unsafe`: defining an Objective-C `NSDraggingSource` class
//! (`objc2::define_class!`), `msg_send!`, and a handful of AppKit calls that
//! `objc2` marks `unsafe`. That `unsafe` is confined to [`dragdrop`]; the rest of
//! the crate stays wrapper-only.
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
// This crate keeps `unsafe` (CGEvent/AppKit C APIs) so it can't adopt
// `[lints] workspace = true` (that would pull in `unsafe_code = "forbid"`).
// Replicate the workspace panic-free clippy denies here instead (audit R2).
#![deny(clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]

pub mod adapter;
pub mod capture;
pub mod clipboard;
pub mod display_info;
pub mod dragdrop;
pub mod inject;
pub mod injector;
pub mod keymap;
pub mod keymap_capture;
pub mod permission;
pub mod tray;

pub use adapter::MacCapture;
pub use capture::{install_listen_only_tap, CaptureError};
pub use clipboard::{ClipboardWriteFailed, MacClipboard};
pub use display_info::{
    active_display_bounds, display_bounds, display_for_global_point, main_display_bounds,
    DisplayBounds,
};
pub use dragdrop::{
    begin_file_drag, read_dragged_file_urls, read_dragged_file_urls_from, write_file_urls,
    DragError, DragSession,
};
pub use inject::{
    button, cursor_position, key_press, left_click, move_cursor, move_cursor_rel, scroll,
    set_cursor_visible, InjectError,
};
pub use injector::MacInjector;
pub use keymap::{
    hid_usage_to_cgkeycode, mods_to_cgflags, mods_to_cgkeycodes, supported_hid_usages,
};
pub use keymap_capture::{
    cgkeycode_to_hid_usage, cursor_moved_for_global, flags_changed_event, to_local_event,
    ModifierState,
};
pub use permission::{
    accessibility_trusted, input_monitoring_trusted, prompt_accessibility, prompt_input_monitoring,
};
pub use tray::{state_label, state_tooltip, MacTray, TrayError};
