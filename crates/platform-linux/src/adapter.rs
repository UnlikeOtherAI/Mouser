//! `UinputInjector` — the Linux [`InputInjection`] adapter (audit H2).
//!
//! Wraps the [`VirtualDevice`] uinput backend and implements the platform-neutral
//! `mouser_core::InputInjection` trait so the engine can drive Linux injection
//! through the same contract as macOS/Windows. The free functions on
//! [`VirtualDevice`] are the private bodies; this struct adapts types
//! (HID usage → evdev `Key`, `ScrollUnit`, button index) and serializes writes.
//!
//! Linux-only: it needs `/dev/uinput` and `input_linux::Key`.

use std::sync::{Mutex, MutexGuard, PoisonError};

use input_linux::Key;
use mouser_core::platform::{InputInjection, PlatformError, PlatformResult, ScrollUnit};

use crate::keymap::{hid_usage_to_evdev, mods_to_evdev};
use crate::uinput::{Button, VirtualDevice, ABS_MAX};

/// macOS/Windows-parity error for an unmapped HID usage.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnmappedKey(pub u16);

impl std::fmt::Display for UnmappedKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "HID usage {:#06x} has no evdev key mapping", self.0)
    }
}

impl std::error::Error for UnmappedKey {}

/// A bad button index (only 0..=4 are defined, §7.5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnknownButton(pub u8);

impl std::fmt::Display for UnknownButton {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "pointer button index {} is not defined (§7.5)", self.0)
    }
}

impl std::error::Error for UnknownButton {}

/// Linux input injector backed by a uinput virtual device.
///
/// `move_cursor` receives display-local **logical pixels**; uinput has no notion
/// of displays, so the injector scales `(x, y)` against a virtual-desktop bound
/// into the device's normalized `0..=ABS_MAX` absolute range. The `display_id` is
/// accepted for contract parity; true per-display routing on Linux needs the
/// compositor layout (Wayland/X11) and is out of scope for this adapter — it maps
/// into the single global pointer space (documented limitation, cf. mac M1).
pub struct UinputInjector {
    dev: Mutex<VirtualDevice>,
    desktop_w: i32,
    desktop_h: i32,
}

impl UinputInjector {
    /// Create the virtual device with a pass-through coordinate bound
    /// (`x,y` clamped straight into `0..=ABS_MAX`).
    ///
    /// # Errors
    /// Propagates [`VirtualDevice::new`] failures (e.g. `/dev/uinput` `EACCES`).
    pub fn new() -> PlatformResult<Self> {
        Self::with_desktop_bounds(ABS_MAX, ABS_MAX)
    }

    /// Create the virtual device, scaling logical pixels against a
    /// `desktop_w × desktop_h` bound into the normalized absolute range.
    ///
    /// # Errors
    /// Propagates [`VirtualDevice::new`] failures.
    pub fn with_desktop_bounds(desktop_w: i32, desktop_h: i32) -> PlatformResult<Self> {
        let dev = VirtualDevice::new().map_err(boxed)?;
        Ok(Self {
            dev: Mutex::new(dev),
            desktop_w: desktop_w.max(1),
            desktop_h: desktop_h.max(1),
        })
    }

    fn scale(&self, v: i32, bound: i32) -> i32 {
        let v = v.clamp(0, bound);
        ((v as i64 * ABS_MAX as i64) / bound as i64) as i32
    }
}

fn boxed<E: std::error::Error + Send + Sync + 'static>(e: E) -> PlatformError {
    Box::new(e)
}

/// Lock the device mutex, recovering the guard if a previous holder panicked.
///
/// A poisoned uinput mutex only means a prior injection unwound mid-write; the
/// `VirtualDevice` itself is still a valid handle, so we recover via
/// [`PoisonError::into_inner`] rather than `.expect(...)` — keeping the runtime
/// path panic-free (mirrors `platform-mac`'s `lock_recover`).
fn lock_recover<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

fn button_of(index: u8) -> Result<Button, UnknownButton> {
    match index {
        0 => Ok(Button::Left),
        1 => Ok(Button::Right),
        2 => Ok(Button::Middle),
        3 => Ok(Button::Back),
        4 => Ok(Button::Forward),
        other => Err(UnknownButton(other)),
    }
}

impl InputInjection for UinputInjector {
    fn move_cursor(&self, _display_id: u32, x: i32, y: i32) -> PlatformResult<()> {
        let ax = self.scale(x, self.desktop_w);
        let ay = self.scale(y, self.desktop_h);
        lock_recover(&self.dev).move_abs(ax, ay).map_err(boxed)
    }

    fn move_cursor_relative(&self, dx: i32, dy: i32) -> PlatformResult<()> {
        lock_recover(&self.dev).move_rel(dx, dy).map_err(boxed)
    }

    fn button(&self, button: u8, down: bool) -> PlatformResult<()> {
        let b = button_of(button).map_err(boxed)?;
        lock_recover(&self.dev).button(b, down).map_err(boxed)
    }

    fn key(&self, usage: u16, down: bool, mods: u16) -> PlatformResult<()> {
        let key: Key = hid_usage_to_evdev(usage).ok_or_else(|| boxed(UnmappedKey(usage)))?;
        let dev = lock_recover(&self.dev);
        // Press modifiers before the key (and release after) so chords land as a
        // real combination, mirroring how a hardware keyboard reports them.
        let modifiers = mods_to_evdev(mods);
        if down {
            for m in &modifiers {
                dev.key(*m, true).map_err(boxed)?;
            }
            dev.key(key, true).map_err(boxed)?;
        } else {
            dev.key(key, false).map_err(boxed)?;
            for m in modifiers.iter().rev() {
                dev.key(*m, false).map_err(boxed)?;
            }
        }
        Ok(())
    }

    fn scroll(&self, dx: i32, dy: i32, unit: ScrollUnit) -> PlatformResult<()> {
        // evdev `REL_WHEEL` is in detents; `Detent120` arrives in 1/120 units, so
        // convert to whole detents. `LogicalPixel` is injected as-is (best-effort;
        // hi-res wheel axes would refine this).
        let (sx, sy) = match unit {
            ScrollUnit::Detent120 => (dx / 120, dy / 120),
            ScrollUnit::LogicalPixel => (dx, dy),
        };
        lock_recover(&self.dev).scroll(sx, sy).map_err(boxed)
    }
}
