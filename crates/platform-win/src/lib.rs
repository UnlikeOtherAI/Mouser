//! `platform-win` — Windows input-injection **skeleton** for Mouser.
//!
//! ## Status: de-risking skeleton
//! Standalone skeleton mirroring `platform-mac` / `platform-linux`. It does
//! **not** depend on `mouser-core`'s `InputCapture` / `InputInjection` traits
//! yet — the goal is to prove the `SendInput` injection path before committing
//! to a trait shape. Reconciliation is a later, separate step.
//!
//! It deliberately does **not** set `[lints] workspace = true`: the workspace
//! forbids `unsafe_code`, but the Windows path calls Win32 (`SendInput`,
//! `GetCursorPos`, `GetSystemMetrics`) which requires `unsafe`.
//!
//! ## What builds where
//! - [`keymap`] is **platform-neutral pure Rust** (no `windows` dependency), so
//!   it compiles and its tests run on **every** host — that is how the Windows
//!   half of Appendix B (HID usage → scancode/VK) is verified on a macOS/Linux
//!   CI box where the rest of the crate is only a stub.
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
//! - **Capture** (future): low-level hooks (`WH_KEYBOARD_LL` / `WH_MOUSE_LL`)
//!   or Raw Input — not part of this injection skeleton.
//!
//! Absolute coordinates are integer logical pixels in the multi-monitor
//! **virtual-desktop** space, matching the wire protocol's absolute
//! `PointerMotion` convention (§7.6); see [`inject::move_cursor`].

// `keymap` is pure logic with no platform deps — always compiled so the
// Appendix B Windows table is testable on any host.
pub mod keymap;
pub use keymap::{hid_usage_to_scancode, hid_usage_to_vk, supported_hid_usages, ScanCode};

#[cfg(target_os = "windows")]
pub mod inject;
#[cfg(target_os = "windows")]
pub use inject::{
    button, cursor_position, key, move_cursor, scroll, Button, InjectError, ScrollUnit,
};

/// Non-Windows stub so the crate compiles on macOS / Linux hosts.
///
/// The `SendInput` backend only exists on Windows; everywhere else this marker
/// keeps the crate buildable without pulling in Win32 APIs. (The [`keymap`]
/// module is still compiled and tested on those hosts.)
#[cfg(not(target_os = "windows"))]
pub const UNSUPPORTED: &str = "platform-win SendInput backend is only available on Windows";
