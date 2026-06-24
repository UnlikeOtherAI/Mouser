//! platform-linux — the Linux input adapters (injection + capture).
//!
//! - [`UinputInjector`] implements `mouser_core::InputInjection` (audit H2) over
//!   a virtual mouse + keyboard created through `/dev/uinput` (the
//!   [`VirtualDevice`] backend) — the *target* side of a handoff.
//! - [`LinuxCapture`] implements `mouser_core::InputCapture` (audit H3) over the
//!   raw evdev devices (`/dev/input/event*`), grabbing them (`EVIOCGRAB`) when the
//!   engine asks to suppress local input — the *source* side.
//! - [`display`] resolves X11 RandR outputs and the real XQueryPointer cursor so
//!   Linux motion uses the same display-local coordinate model as macOS/Windows.
//! - [`keymap`] is the Linux HID↔evdev table (audit H11); its host-independent
//!   surface (`supported_hid_usages`, `hid_usage_to_evdev_code`,
//!   `evdev_code_to_hid_usage`) lets the cross-platform keymap round-trip parity
//!   test run on a macOS build host.
//!
//! The uinput/evdev backends and the adapters are Linux-only and live in
//! [`uinput`] / [`adapter`] / [`capture`]. On other hosts (macOS/Windows) the
//! crate still builds: only the keymap's pure-data surface and the stub below
//! compile, so `cargo build -p platform-linux` succeeds everywhere.

// This crate keeps `unsafe` (uinput / evdev) so it can't adopt
// `[lints] workspace = true` (that would pull in `unsafe_code = "forbid"`).
// Replicate the workspace panic-free clippy denies here instead (audit R2).
#![deny(clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]

pub mod clipboard;
pub mod keymap;

pub use clipboard::LinuxClipboard;

#[cfg(target_os = "linux")]
pub mod adapter;
#[cfg(target_os = "linux")]
pub mod capture;
#[cfg(target_os = "linux")]
mod capture_translate;
#[cfg(target_os = "linux")]
pub mod display;
#[cfg(target_os = "linux")]
pub mod uinput;

#[cfg(target_os = "linux")]
pub use adapter::UinputInjector;
#[cfg(target_os = "linux")]
pub use capture::LinuxCapture;
#[cfg(target_os = "linux")]
pub use display::{active_display_bounds, display_bounds, DisplayBounds};
#[cfg(target_os = "linux")]
pub use input_linux::Key;
#[cfg(target_os = "linux")]
pub use uinput::{Button, VirtualDevice, ABS_MAX, DEVICE_NAME};

/// Non-Linux stub so the crate compiles on macOS / Windows hosts.
///
/// The uinput backend only exists on Linux; everywhere else this marker keeps
/// the crate buildable without pulling in Linux-only system APIs. The [`keymap`]
/// module's pure-data coverage surface is available on every host.
#[cfg(not(target_os = "linux"))]
pub const UNSUPPORTED: &str = "platform-linux uinput backend is only available on Linux";
