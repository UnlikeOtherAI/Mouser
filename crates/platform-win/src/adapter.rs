//! Windows adapter implementing `mouser_core` platform traits.
//!
//! [`WinInjector`] wraps the `SendInput` backend. [`WinCapture`] is re-exported
//! from the capture module so the historical `adapter::WinCapture` path remains
//! available while capture stays in its own focused source file.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

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
///
/// `saved_cursor` holds the pre-hide cursor position while the peer owns input,
/// so `set_cursor_visible(true)` can restore it. See [`WinInjector::set_cursor_visible`]
/// for the crash-safe rationale behind parking (rather than `SetSystemCursor`-style
/// system-wide hiding).
#[derive(Debug, Default)]
pub struct WinInjector {
    /// Physical-pixel cursor position captured at the last `set_cursor_visible(false)`,
    /// or `None` when the cursor is shown. `None` is the "visible" state, so a fresh
    /// (or double-shown) injector starts visible.
    saved_cursor: Mutex<Option<(i32, i32)>>,
    /// Whether this visible ownership period has already asserted the Win32 cursor
    /// display counter. Reset when ownership leaves this Windows machine.
    cursor_show_asserted: AtomicBool,
}

impl WinInjector {
    /// Create a `SendInput` injector with the cursor in the visible state.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    fn assert_cursor_visible_once(&self) {
        if !self.cursor_show_asserted.swap(true, Ordering::AcqRel) {
            inject::force_cursor_visible();
        }
    }
}

/// The next saved-cursor state given the current one and the requested visibility.
///
/// Pure bookkeeping for [`WinInjector::set_cursor_visible`], split out so the
/// idempotency rules can be unit-tested without any Win32 call:
///
/// - hide while already hidden (`current = Some`) is a no-op — keep the *original*
///   saved position, never overwrite it with the parked corner;
/// - hide while visible (`current = None`) saves `live_pos`, to be parked;
/// - show while hidden restores and clears to `None`;
/// - show while already visible (`current = None`) is a no-op.
///
/// Returns `(next_saved, action)` where `action` tells the caller what Win32 work
/// to perform.
fn next_cursor_state(
    current: Option<(i32, i32)>,
    visible: bool,
    live_pos: Option<(i32, i32)>,
) -> (Option<(i32, i32)>, CursorAction) {
    match (visible, current) {
        // Hide while visible: remember where the cursor is, then park it.
        (false, None) => (live_pos, CursorAction::Park),
        // Hide while already hidden: keep the original saved position untouched.
        (false, Some(saved)) => (Some(saved), CursorAction::None),
        // Show while hidden: restore to the saved position and clear it.
        (true, Some(saved)) => (None, CursorAction::Restore(saved)),
        // Show while already visible: nothing to do.
        (true, None) => (None, CursorAction::None),
    }
}

/// What [`WinInjector::set_cursor_visible`] must do to the OS cursor after
/// [`next_cursor_state`] decides the bookkeeping.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CursorAction {
    /// No OS-cursor movement (idempotent double-hide / double-show).
    None,
    /// Park the cursor at the virtual-desktop corner (hide).
    Park,
    /// Move the cursor back to the carried physical-pixel position (show).
    Restore((i32, i32)),
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

/// Bounds plus primary-display metadata from Windows monitor enumeration.
#[derive(Debug, Clone, Copy, PartialEq)]
struct MonitorRect {
    rect: RECT,
    primary: bool,
}

impl MonitorRect {
    fn sort_key(self) -> (bool, i32, i32, i32, i32) {
        (
            !self.primary,
            self.rect.top,
            self.rect.left,
            self.rect.right,
            self.rect.bottom,
        )
    }
}

fn order_monitors(monitors: &mut [MonitorRect]) {
    monitors.sort_by_key(|m| m.sort_key());
}

/// Enumerate active monitors with the Windows primary display as id 0.
///
/// The wire protocol currently sends target pointer motion with `display_id = 0`;
/// on Windows that must mean the user's primary display, not whichever monitor is
/// top-left in the virtual desktop.
pub fn active_display_bounds() -> PlatformResult<Vec<DisplayBounds>> {
    let mut monitors: Vec<MonitorRect> = Vec::new();
    let ok = unsafe {
        EnumDisplayMonitors(
            None,
            None,
            Some(enum_monitor),
            LPARAM((&mut monitors as *mut Vec<MonitorRect>) as isize),
        )
    };
    if !ok.as_bool() {
        return Err(Box::new(windows::core::Error::from_thread()));
    }

    order_monitors(&mut monitors);
    let mut displays = Vec::with_capacity(monitors.len());
    for monitor in monitors {
        let id = displays.len() as u32;
        if let Some(bounds) = DisplayBounds::from_rect(id, monitor.rect) {
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
    const MONITORINFOF_PRIMARY: u32 = 0x0000_0001;

    let monitors = unsafe { &mut *(data.0 as *mut Vec<MonitorRect>) };
    let mut info = MONITORINFO {
        cbSize: std::mem::size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    if unsafe { GetMonitorInfoW(monitor, &mut info) }.as_bool() {
        monitors.push(MonitorRect {
            rect: info.rcMonitor,
            primary: (info.dwFlags & MONITORINFOF_PRIMARY) != 0,
        });
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
        self.assert_cursor_visible_once();
        let bounds = display_bounds(display_id)?;
        let (vx, vy) = bounds.local_to_virtual(x, y);
        inject::move_cursor(vx, vy).map_err(boxed)
    }

    fn move_cursor_relative(&self, dx: i32, dy: i32) -> PlatformResult<()> {
        self.assert_cursor_visible_once();
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

    /// Show or hide the local cursor while the peer owns input.
    ///
    /// ## Why parking, not a system cursor hide
    /// Windows has no clean *per-process* "hide the global cursor" call: `ShowCursor`
    /// only affects our own window-message queue, and `SetSystemCursor` (and the old
    /// `ClipCursor` pin) mutate **system-wide** state that *survives process death* —
    /// a crash, panic, or force-kill mid-handoff would strand the user with an invisible
    /// or clamped pointer that an idle relaunch does not clear. A prior `ClipCursor` pin
    /// caused exactly that ("unusable mouse"); this method deliberately avoids that class
    /// of bug.
    ///
    /// Instead we *park* the cursor: on hide we save the current position
    /// (`GetCursorPos`) and `SetCursorPos` it to the bottom-right-most pixel of the
    /// virtual desktop; on show we move it back. `SetCursorPos` is ordinary,
    /// non-persistent per-call state. If the process dies while hidden, the cursor is
    /// simply sitting in a corner — fully recoverable by moving the mouse, with no
    /// lingering system setting to undo.
    ///
    /// Idempotent: a double-hide keeps the *original* saved position (it does not
    /// re-save the parked corner), and a double-show is a no-op.
    fn set_cursor_visible(&self, visible: bool) -> PlatformResult<()> {
        if visible {
            self.assert_cursor_visible_once();
        }

        // Read the live position *before* taking the lock decision only when hiding;
        // `next_cursor_state` ignores `live_pos` on every other branch, so an error
        // reading it there must not fail the call.
        let live_pos = if visible {
            None
        } else {
            let p = inject::cursor_position().map_err(boxed)?;
            Some((p.x, p.y))
        };

        // Hold the lock across the decision + the OS move so concurrent ownership
        // transitions can't interleave a save/restore (poisoned lock => surface it).
        let mut saved = self
            .saved_cursor
            .lock()
            .map_err(|_| boxed(CursorLockPoisoned))?;
        let (next, action) = next_cursor_state(*saved, visible, live_pos);

        match action {
            CursorAction::None => {}
            CursorAction::Park => {
                let (px, py) = inject::park_position().map_err(boxed)?;
                inject::set_cursor_pos(px, py).map_err(boxed)?;
            }
            CursorAction::Restore((rx, ry)) => {
                inject::set_cursor_pos(rx, ry).map_err(boxed)?;
            }
        }
        // Commit the bookkeeping only after the OS move succeeded, so a failed park
        // leaves us in the (truthful) visible state rather than claiming a save we
        // never parked.
        *saved = next;
        if !visible {
            self.cursor_show_asserted.store(false, Ordering::Release);
        }
        Ok(())
    }
}

/// The `saved_cursor` mutex was poisoned by a panic in another holder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CursorLockPoisoned;

impl std::fmt::Display for CursorLockPoisoned {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "saved-cursor lock poisoned")
    }
}

impl std::error::Error for CursorLockPoisoned {}

#[cfg(test)]
#[path = "adapter_tests.rs"]
mod tests;
