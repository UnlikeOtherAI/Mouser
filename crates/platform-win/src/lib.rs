//! `platform-win` — Windows platform adapter for Mouser.
//!
//! [`adapter::WinInjector`] implements `mouser_core::InputInjection` over the
//! `SendInput` backend in [`inject`]. [`adapter::WinCapture`] implements
//! `mouser_core::InputCapture` as a `CaptureMode` state machine: passive
//! `GetCursorPos` edge sensing while connected (no hooks, no suppression) and
//! low-level keyboard/mouse hooks only while actively forwarding to a peer.
//! [`keymap`] is the Windows HID↔scancode/VK table, and [`clipboard`] is the Win32
//! clipboard adapter.
//!
//! It deliberately does **not** set `[lints] workspace = true`: the workspace
//! forbids `unsafe_code`, but the Windows path calls Win32 (`SendInput`,
//! `GetCursorPos`, `GetSystemMetrics`) which requires `unsafe`.
//!
//! ## What builds where
//! - [`keymap`] is **platform-neutral pure Rust** (no `windows` dependency), so
//!   it compiles and its tests run on **every** host — that is how the Windows
//!   half of Appendix B (HID usage → scancode/VK) is verified on every host.
//! - [`inject`] (the real `SendInput` work) is `#[cfg(target_os = "windows")]`.
//!   On non-Windows hosts only [`UNSUPPORTED`] is compiled, so
//!   `cargo build -p platform-win` succeeds everywhere.
//!
//! ## Capability reality (see `docs/tech-stack.md` §4, `docs/windows-build.md`)
//! - **Injection** (`inject`): `SendInput` works for normal foreground apps.
//!   **UIPI** blocks injection into higher-integrity (elevated) windows unless
//!   the injector is elevated or signed with `uiAccess`. The **UAC secure
//!   desktop** and **lock screen** are a separate desktop an ordinary process
//!   cannot reach. In both cases events are silently dropped (no error). The
//!   adapter surfaces this as `CapState::SecureContext` /
//!   `BlockedReason::SecureDesktop` (§7.4) and returns ownership to the source.
//! - **Capture** (`adapter::WinCapture`): low-level hooks (`WH_KEYBOARD_LL` /
//!   `WH_MOUSE_LL`) observe local keyboard, pointer, button, and wheel input and
//!   can suppress events when the engine returns `CaptureDecision::Suppress`.
//!
//! Absolute coordinates are integer logical pixels in the multi-monitor
//! **virtual-desktop** space, matching the wire protocol's absolute
//! `PointerMotion` convention (§7.6); see [`inject::move_cursor`].

// This crate keeps `unsafe` (Win32 SendInput / clipboard) so it can't adopt
// `[lints] workspace = true` (that would pull in `unsafe_code = "forbid"`).
// Replicate the workspace panic-free clippy denies here instead (audit R2).
#![deny(clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]

// `keymap` is pure logic with no platform deps — always compiled so the
// Appendix B Windows table is testable on any host.
pub mod keymap;
pub use keymap::{hid_usage_to_scancode, hid_usage_to_vk, supported_hid_usages, ScanCode};

// `cfhtml` is the pure CF_HTML wrap/unwrap codec (no `windows` dependency). It is
// always compiled so the CF_HTML round-trip is unit-tested on every host even
// though the clipboard adapter that uses it is Windows-only.
mod cfhtml;
mod clipboard_text;

#[cfg(target_os = "windows")]
pub mod adapter;
#[cfg(target_os = "windows")]
pub mod inject;
#[cfg(target_os = "windows")]
pub use adapter::{active_display_bounds, display_bounds, DisplayBounds, WinCapture, WinInjector};
#[cfg(target_os = "windows")]
pub use inject::{
    button, cursor_position, key, move_cursor, move_cursor_relative, scroll, Button, InjectError,
    ScrollUnit,
};

#[cfg(target_os = "windows")]
pub mod clipboard;
#[cfg(target_os = "windows")]
pub use clipboard::{ClipboardError, WinClipboard};

// Off-Windows, expose a `WinClipboard` stub implementing `mouser_core::Clipboard`
// (mirrors `platform-linux`'s `LinuxClipboard` stub) so cross-platform code and
// tests can name and exercise the type on every host.
#[cfg(not(target_os = "windows"))]
pub mod clipboard_stub;
#[cfg(not(target_os = "windows"))]
pub use clipboard_stub::WinClipboard;

/// Non-Windows stub so the crate compiles on macOS / Linux hosts.
///
/// The `SendInput` backend only exists on Windows; everywhere else this marker
/// keeps the crate buildable without pulling in Win32 APIs. (The [`keymap`]
/// module is still compiled and tested on those hosts.)
#[cfg(not(target_os = "windows"))]
pub const UNSUPPORTED: &str = "platform-win SendInput backend is only available on Windows";
