//! Windows adapter implementing `mouser_core` platform traits.
//!
//! [`WinInjector`] wraps the `SendInput` backend. [`WinCapture`] is re-exported
//! from the capture module so the historical `adapter::WinCapture` path remains
//! available while capture stays in its own focused source file.

use mouser_core::platform::{
    InputInjection, PlatformError, PlatformResult, ScrollUnit as CoreScrollUnit,
};
use windows::Win32::Foundation::{LPARAM, RECT};
use windows::Win32::Graphics::Gdi::{
    EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFO,
};

pub use crate::capture::{
    emergency_reclaim_chord_from_mods, is_emergency_reclaim_event, CaptureAlreadyRunning,
    CaptureStartFailed, WinCapture,
};
use crate::inject::{self, Button, ScrollUnit as WinScrollUnit};

/// Windows input injector backed by `SendInput`.
#[derive(Debug, Default, Clone, Copy)]
pub struct WinInjector;

impl WinInjector {
    /// Create a stateless `SendInput` injector.
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

/// Bounds of one active monitor in Windows' virtual-desktop coordinate space.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DisplayBounds {
    /// Zero-based id from the current stable enumeration order.
    pub id: u32,
    /// Left edge in virtual-desktop pixels.
    pub left: i32,
    /// Top edge in virtual-desktop pixels.
    pub top: i32,
    /// Width in pixels.
    pub width: i32,
    /// Height in pixels.
    pub height: i32,
}

impl DisplayBounds {
    fn from_rect(id: u32, rect: RECT) -> Option<Self> {
        let width = rect.right - rect.left;
        let height = rect.bottom - rect.top;
        if width <= 0 || height <= 0 {
            return None;
        }
        Some(Self {
            id,
            left: rect.left,
            top: rect.top,
            width,
            height,
        })
    }

    fn local_to_virtual(self, x: i32, y: i32) -> (i32, i32) {
        let x = x.clamp(0, self.width - 1);
        let y = y.clamp(0, self.height - 1);
        (self.left + x, self.top + y)
    }

    pub(crate) fn contains_virtual(self, x: i32, y: i32) -> bool {
        x >= self.left && x < self.left + self.width && y >= self.top && y < self.top + self.height
    }

    pub(crate) fn virtual_to_local(self, x: i32, y: i32) -> (i32, i32) {
        (x - self.left, y - self.top)
    }
}

/// Enumerate active monitors in deterministic top-left order.
pub fn active_display_bounds() -> PlatformResult<Vec<DisplayBounds>> {
    let mut rects: Vec<RECT> = Vec::new();
    let ok = unsafe {
        EnumDisplayMonitors(
            None,
            None,
            Some(enum_monitor),
            LPARAM((&mut rects as *mut Vec<RECT>) as isize),
        )
    };
    if !ok.as_bool() {
        return Err(Box::new(windows::core::Error::from_thread()));
    }

    rects.sort_by_key(|r| (r.top, r.left, r.right, r.bottom));
    let mut displays = Vec::with_capacity(rects.len());
    for rect in rects {
        let id = displays.len() as u32;
        if let Some(bounds) = DisplayBounds::from_rect(id, rect) {
            displays.push(bounds);
        }
    }
    Ok(displays)
}

/// Return the current bounds for a display id.
pub fn display_bounds(display_id: u32) -> PlatformResult<DisplayBounds> {
    active_display_bounds()?
        .into_iter()
        .find(|b| b.id == display_id)
        .ok_or_else(|| boxed(UnknownDisplay(display_id)))
}

unsafe extern "system" fn enum_monitor(
    monitor: HMONITOR,
    _hdc: HDC,
    _rect: *mut RECT,
    data: LPARAM,
) -> windows::core::BOOL {
    let rects = unsafe { &mut *(data.0 as *mut Vec<RECT>) };
    let mut info = MONITORINFO {
        cbSize: std::mem::size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    if unsafe { GetMonitorInfoW(monitor, &mut info) }.as_bool() {
        rects.push(info.rcMonitor);
    }
    true.into()
}

/// Error when a wire `display_id` matches no active monitor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnknownDisplay(pub u32);

impl std::fmt::Display for UnknownDisplay {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "no active Windows display with id {}", self.0)
    }
}

impl std::error::Error for UnknownDisplay {}

/// A bad button index (only 0..=4 are defined, §7.5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnknownButton(pub u8);

impl std::fmt::Display for UnknownButton {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "pointer button index {} is not defined (§7.5)", self.0)
    }
}

impl std::error::Error for UnknownButton {}

fn button_of(index: u8) -> Result<Button, UnknownButton> {
    Button::from_wire(index).ok_or(UnknownButton(index))
}

fn scroll_unit(unit: CoreScrollUnit) -> WinScrollUnit {
    match unit {
        CoreScrollUnit::Detent120 => WinScrollUnit::Detent120,
        CoreScrollUnit::LogicalPixel => WinScrollUnit::LogicalPixel,
    }
}

fn boxed<E: std::error::Error + Send + Sync + 'static>(e: E) -> PlatformError {
    Box::new(e)
}

fn modifier_usages(mods: u16) -> Vec<u16> {
    const MOD_BITS: [(u16, u16); 8] = [
        (0, 0xE0),
        (1, 0xE1),
        (2, 0xE2),
        (3, 0xE3),
        (4, 0xE4),
        (5, 0xE5),
        (6, 0xE6),
        (7, 0xE7),
    ];
    let mut out = Vec::new();
    for (bit, usage) in MOD_BITS {
        if mods & (1 << bit) != 0 {
            out.push(usage);
        }
    }
    out
}

impl InputInjection for WinInjector {
    fn move_cursor(&self, display_id: u32, x: i32, y: i32) -> PlatformResult<()> {
        let bounds = display_bounds(display_id)?;
        let (vx, vy) = bounds.local_to_virtual(x, y);
        inject::move_cursor(vx, vy).map_err(boxed)
    }

    fn move_cursor_relative(&self, dx: i32, dy: i32) -> PlatformResult<()> {
        inject::move_cursor_relative(dx, dy).map_err(boxed)
    }

    fn button(&self, button: u8, down: bool) -> PlatformResult<()> {
        let button = button_of(button).map_err(boxed)?;
        inject::button(button, down).map_err(boxed)
    }

    fn key(&self, usage: u16, down: bool, mods: u16) -> PlatformResult<()> {
        let modifiers = modifier_usages(mods);
        if down {
            for m in &modifiers {
                inject::key(*m, true).map_err(boxed)?;
            }
            inject::key(usage, true).map_err(boxed)?;
        } else {
            inject::key(usage, false).map_err(boxed)?;
            for m in modifiers.iter().rev() {
                inject::key(*m, false).map_err(boxed)?;
            }
        }
        Ok(())
    }

    fn scroll(&self, dx: i32, dy: i32, unit: CoreScrollUnit) -> PlatformResult<()> {
        inject::scroll(dx, dy, scroll_unit(unit)).map_err(boxed)
    }
}

#[cfg(test)]
#[path = "adapter_tests.rs"]
mod tests;
