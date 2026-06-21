//! platform-linux — Linux input-injection spike.
//!
//! Standalone (does **not** depend on `mouser-core` traits yet). Creates a
//! virtual mouse + keyboard through `/dev/uinput` using the `input-linux`
//! crate and exposes a tiny injection surface: [`VirtualDevice::move_rel`],
//! [`VirtualDevice::button`], [`VirtualDevice::key`].
//!
//! All real work is Linux-only and lives in [`uinput`]. On other hosts
//! (macOS/Windows) the crate still builds: only the stub below is compiled, so
//! `cargo build -p platform-linux` succeeds everywhere.

#[cfg(target_os = "linux")]
pub mod uinput;

#[cfg(target_os = "linux")]
pub use input_linux::Key;
#[cfg(target_os = "linux")]
pub use uinput::{Button, VirtualDevice, DEVICE_NAME};

/// Non-Linux stub so the crate compiles on macOS / Windows hosts.
///
/// The uinput backend only exists on Linux; everywhere else this marker keeps
/// the crate buildable without pulling in Linux-only system APIs.
#[cfg(not(target_os = "linux"))]
pub const UNSUPPORTED: &str = "platform-linux uinput backend is only available on Linux";
