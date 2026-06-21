//! platform-linux — the Linux input-injection adapter.
//!
//! [`UinputInjector`] implements `mouser_core::InputInjection` (audit H2) over a
//! virtual mouse + keyboard created through `/dev/uinput` (the [`VirtualDevice`]
//! backend). [`keymap`] is the Linux HID↔evdev table (audit H11); its
//! host-independent `supported_hid_usages` lets the cross-platform keymap parity
//! test run on a macOS build host.
//!
//! The uinput backend and the adapter are Linux-only and live in [`uinput`] /
//! [`adapter`]. On other hosts (macOS/Windows) the crate still builds: only the
//! keymap's pure-data surface and the stub below compile, so
//! `cargo build -p platform-linux` succeeds everywhere.

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
pub mod uinput;

#[cfg(target_os = "linux")]
pub use adapter::UinputInjector;
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
