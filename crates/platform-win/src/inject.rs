//! Windows input **injection** via `SendInput` — skeleton.
//!
//! Synthesizes mouse motion, mouse buttons, scroll, and key events through the
//! Win32 [`SendInput`] API. This is the Windows analogue of `platform-mac`'s
//! `inject` (Core Graphics) and `platform-linux`'s uinput backend.
//!
//! ## Coordinate space (absolute motion)
//! `SendInput` absolute coordinates are **normalized** to `0..=65535` over a
//! rectangle, not raw pixels. With `MOUSEEVENTF_VIRTUALDESK | MOUSEEVENTF_ABSOLUTE`
//! that rectangle is the **whole virtual desktop** (all monitors). The wire
//! protocol delivers motion as integer logical pixels in a target *display's*
//! space (§7.6); [`move_cursor`] takes a pixel point in **virtual-desktop**
//! coordinates and normalizes it. Mapping a per-display `(display_id, x, y)`
//! into virtual-desktop pixels is the job of the (future) display-enumeration
//! layer; this skeleton handles the final normalize+inject hop.
//!
//! ## Keys use scancodes
//! [`key`] injects with `KEYEVENTF_SCANCODE` (+ `KEYEVENTF_EXTENDEDKEY` for
//! extended keys), driven by [`crate::keymap::hid_usage_to_scancode`]. Scancodes
//! name the *physical* key independent of the active keyboard layout, matching
//! the wire spec's physical-key semantics (§7.5).
//!
//! ## Injection reality (see `docs/tech-stack.md` §4, `docs/windows-build.md`)
//! `SendInput` succeeds for normal foreground apps, but **UIPI** (User Interface
//! Privilege Isolation) silently blocks injection into a window owned by a
//! *higher-integrity* process (an elevated/admin app) unless the injector is
//! elevated or holds the `uiAccess` flag. The **UAC secure desktop** and the
//! **lock screen** run on a separate desktop that an ordinary process cannot
//! reach at all. In those cases `SendInput` returns the number of events queued
//! (often non-zero) yet nothing lands — there is no error code. The adapter must
//! surface this as `CapState::SecureContext` / `BlockedReason::SecureDesktop`
//! (§7.4) and return ownership to the source. See `docs/windows-build.md` for
//! the optional signed `uiAccess` helper that lifts the UIPI limit.

use windows::Win32::Foundation::POINT;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE, KEYBDINPUT, KEYBD_EVENT_FLAGS,
    KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP, KEYEVENTF_SCANCODE, MOUSEEVENTF_ABSOLUTE,
    MOUSEEVENTF_HWHEEL, MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP, MOUSEEVENTF_MIDDLEDOWN,
    MOUSEEVENTF_MIDDLEUP, MOUSEEVENTF_MOVE, MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP,
    MOUSEEVENTF_VIRTUALDESK, MOUSEEVENTF_WHEEL, MOUSEEVENTF_XDOWN, MOUSEEVENTF_XUP, MOUSEINPUT,
    MOUSE_EVENT_FLAGS, VIRTUAL_KEY,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetCursorPos, GetSystemMetrics, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN,
    SM_YVIRTUALSCREEN, XBUTTON1, XBUTTON2,
};

use crate::keymap::hid_usage_to_scancode;

/// Mouse buttons this skeleton can synthesize.
///
/// Numeric values mirror the wire `PointerButton.button` field (§7.5):
/// `0=left, 1=right, 2=middle, 3=back, 4=forward`.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Button {
    /// Primary (left) button — wire `0`.
    Left,
    /// Secondary (right) button — wire `1`.
    Right,
    /// Middle / wheel button — wire `2`.
    Middle,
    /// "Back" / X1 button — wire `3`.
    Back,
    /// "Forward" / X2 button — wire `4`.
    Forward,
}

impl Button {
    /// Map a wire `PointerButton.button` code (§7.5) to a [`Button`].
    #[must_use]
    pub fn from_wire(code: u8) -> Option<Self> {
        match code {
            0 => Some(Self::Left),
            1 => Some(Self::Right),
            2 => Some(Self::Middle),
            3 => Some(Self::Back),
            4 => Some(Self::Forward),
            _ => None,
        }
    }
}

/// Errors from injection calls.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InjectError {
    /// `SendInput` did not queue all events. The `u32` is how many it *did*
    /// accept (Win32 returns the count of successfully inserted events).
    ///
    /// NOTE: a *full* count is **not** proof the input took effect — UIPI /
    /// secure desktop can swallow accepted events silently (see module docs).
    SendInput {
        /// Events successfully inserted into the input stream.
        inserted: u32,
        /// Events we asked `SendInput` to insert.
        requested: u32,
    },
    /// The virtual-desktop metrics came back as zero width/height, so absolute
    /// normalization is impossible (no display, or called too early at boot).
    NoVirtualDesktop,
    /// The HID usage has no Windows scancode mapping yet.
    UnmappedKey(u16),
    /// A Win32 call failed; carries the `windows::core::Error`.
    Win32(windows::core::Error),
}

impl std::fmt::Display for InjectError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SendInput {
                inserted,
                requested,
            } => write!(
                f,
                "SendInput inserted {inserted}/{requested} events (UIPI or secure \
                 desktop may have blocked the rest)"
            ),
            Self::NoVirtualDesktop => {
                write!(
                    f,
                    "virtual desktop has zero size; cannot normalize absolute coords"
                )
            }
            Self::UnmappedKey(u) => write!(f, "HID usage {u:#06x} has no Windows scancode"),
            Self::Win32(e) => write!(f, "Win32 error: {e}"),
        }
    }
}

impl std::error::Error for InjectError {}

/// Read the current cursor position in physical screen pixels.
///
/// Ground-truth oracle for the demo (mirrors `platform-mac::cursor_position`).
///
/// # Errors
/// Returns [`InjectError::Win32`] if `GetCursorPos` fails.
pub fn cursor_position() -> Result<POINT, InjectError> {
    let mut p = POINT::default();
    // SAFETY: `GetCursorPos` writes a valid `POINT`; we pass a live, owned out-ptr.
    unsafe { GetCursorPos(&mut p) }.map_err(InjectError::Win32)?;
    Ok(p)
}

/// Bounds of the whole virtual desktop (all monitors), in physical pixels:
/// `(left, top, width, height)`.
///
/// `left`/`top` can be negative (a monitor left of / above the primary).
fn virtual_desktop() -> Result<(i32, i32, i32, i32), InjectError> {
    // SAFETY: `GetSystemMetrics` is a pure read of a system constant.
    let (left, top, w, h) = unsafe {
        (
            GetSystemMetrics(SM_XVIRTUALSCREEN),
            GetSystemMetrics(SM_YVIRTUALSCREEN),
            GetSystemMetrics(SM_CXVIRTUALSCREEN),
            GetSystemMetrics(SM_CYVIRTUALSCREEN),
        )
    };
    if w <= 0 || h <= 0 {
        return Err(InjectError::NoVirtualDesktop);
    }
    Ok((left, top, w, h))
}

/// Normalize a virtual-desktop pixel coordinate to the `0..=65535` absolute
/// space `SendInput` expects with `MOUSEEVENTF_VIRTUALDESK`.
///
/// Per Win32 docs the normalized value is
/// `((pixel - origin) * 65535) / (extent - 1)`, clamped to `0..=65535`.
fn normalize_axis(pixel: i32, origin: i32, extent: i32) -> i32 {
    let denom = (extent - 1).max(1);
    let n = i64::from(pixel - origin) * 65535 / i64::from(denom);
    n.clamp(0, 65535) as i32
}

/// Move the cursor to an absolute point in **virtual-desktop pixel** coordinates.
///
/// `x,y` are integer pixels in the multi-monitor virtual-desktop space (the
/// same space `GetSystemMetrics(SM_*VIRTUALSCREEN)` describes). They are
/// normalized to `0..=65535` and injected with
/// `MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK`.
///
/// # Errors
/// [`InjectError::NoVirtualDesktop`] if the desktop has zero size, or
/// [`InjectError::SendInput`] if the event was not queued.
pub fn move_cursor(x: i32, y: i32) -> Result<(), InjectError> {
    let (left, top, w, h) = virtual_desktop()?;
    let nx = normalize_axis(x, left, w);
    let ny = normalize_axis(y, top, h);

    let mi = MOUSEINPUT {
        dx: nx,
        dy: ny,
        mouseData: 0,
        dwFlags: MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK,
        time: 0,
        dwExtraInfo: 0,
    };
    send_mouse(mi)
}

/// Press (`down = true`) or release (`down = false`) a mouse button at the
/// current cursor position.
///
/// # Errors
/// [`InjectError::SendInput`] if the event was not queued.
pub fn button(button: Button, down: bool) -> Result<(), InjectError> {
    let (dw_flags, mouse_data) = button_flags(button, down);
    let mi = MOUSEINPUT {
        dx: 0,
        dy: 0,
        mouseData: mouse_data,
        dwFlags: dw_flags,
        time: 0,
        dwExtraInfo: 0,
    };
    send_mouse(mi)
}

/// Resolve the `dwFlags` + `mouseData` for a button event.
///
/// X buttons (back/forward) encode which button in `mouseData` and use the
/// shared `XDOWN`/`XUP` flag.
fn button_flags(button: Button, down: bool) -> (MOUSE_EVENT_FLAGS, u32) {
    match button {
        Button::Left if down => (MOUSEEVENTF_LEFTDOWN, 0),
        Button::Left => (MOUSEEVENTF_LEFTUP, 0),
        Button::Right if down => (MOUSEEVENTF_RIGHTDOWN, 0),
        Button::Right => (MOUSEEVENTF_RIGHTUP, 0),
        Button::Middle if down => (MOUSEEVENTF_MIDDLEDOWN, 0),
        Button::Middle => (MOUSEEVENTF_MIDDLEUP, 0),
        Button::Back if down => (MOUSEEVENTF_XDOWN, u32::from(XBUTTON1)),
        Button::Back => (MOUSEEVENTF_XUP, u32::from(XBUTTON1)),
        Button::Forward if down => (MOUSEEVENTF_XDOWN, u32::from(XBUTTON2)),
        Button::Forward => (MOUSEEVENTF_XUP, u32::from(XBUTTON2)),
    }
}

/// Scroll by wheel deltas (`dx` horizontal, `dy` vertical).
///
/// Wire `Scroll` carries `dx/dy` in either `Detent120` or `LogicalPixel` units
/// (§7.5). `SendInput`'s wheel is natively in `Detent120` units (`WHEEL_DELTA`
/// = 120 per notch), so a `Detent120` value maps through unchanged; a
/// `LogicalPixel` value should be converted by the caller. Vertical uses
/// `MOUSEEVENTF_WHEEL`, horizontal `MOUSEEVENTF_HWHEEL`.
///
/// # Errors
/// [`InjectError::SendInput`] if an event was not queued.
pub fn scroll(dx: i32, dy: i32) -> Result<(), InjectError> {
    if dy != 0 {
        let mi = MOUSEINPUT {
            dx: 0,
            dy: 0,
            mouseData: dy as u32,
            dwFlags: MOUSEEVENTF_WHEEL,
            time: 0,
            dwExtraInfo: 0,
        };
        send_mouse(mi)?;
    }
    if dx != 0 {
        let mi = MOUSEINPUT {
            dx: 0,
            dy: 0,
            mouseData: dx as u32,
            dwFlags: MOUSEEVENTF_HWHEEL,
            time: 0,
            dwExtraInfo: 0,
        };
        send_mouse(mi)?;
    }
    Ok(())
}

/// Press (`down = true`) or release (`down = false`) a key identified by its
/// **HID usage** (Usage Page 0x07, §7.5).
///
/// Injected as a **scancode** (`KEYEVENTF_SCANCODE`, plus
/// `KEYEVENTF_EXTENDEDKEY` for the extended block) so the *physical* key is
/// reproduced regardless of the receiver's keyboard layout.
///
/// # Errors
/// [`InjectError::UnmappedKey`] if the usage has no scancode mapping, or
/// [`InjectError::SendInput`] if the event was not queued.
pub fn key(usage: u16, down: bool) -> Result<(), InjectError> {
    let sc = hid_usage_to_scancode(usage).ok_or(InjectError::UnmappedKey(usage))?;

    let mut flags: KEYBD_EVENT_FLAGS = KEYEVENTF_SCANCODE;
    if sc.extended {
        flags |= KEYEVENTF_EXTENDEDKEY;
    }
    if !down {
        flags |= KEYEVENTF_KEYUP;
    }

    let ki = KEYBDINPUT {
        wVk: VIRTUAL_KEY(0), // ignored when KEYEVENTF_SCANCODE is set
        wScan: sc.code,
        dwFlags: flags,
        time: 0,
        dwExtraInfo: 0,
    };
    let input = INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 { ki },
    };
    send(&[input])
}

/// Send a single mouse `INPUT`.
fn send_mouse(mi: MOUSEINPUT) -> Result<(), InjectError> {
    let input = INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 { mi },
    };
    send(&[input])
}

/// Thin wrapper over `SendInput` that maps a short write to [`InjectError`].
///
/// Returns `Ok(())` only when every event was queued. A full count still does
/// not guarantee the events *took effect* (UIPI / secure desktop) — see module
/// docs.
fn send(inputs: &[INPUT]) -> Result<(), InjectError> {
    let requested = inputs.len() as u32;
    // SAFETY: `inputs` is a live slice of correctly-initialized `INPUT` unions
    // and `size_of::<INPUT>()` is the matching stride Win32 requires.
    let inserted = unsafe { SendInput(inputs, std::mem::size_of::<INPUT>() as i32) };
    if inserted == requested {
        Ok(())
    } else {
        Err(InjectError::SendInput {
            inserted,
            requested,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn button_from_wire_covers_catalog() {
        assert_eq!(Button::from_wire(0), Some(Button::Left));
        assert_eq!(Button::from_wire(1), Some(Button::Right));
        assert_eq!(Button::from_wire(2), Some(Button::Middle));
        assert_eq!(Button::from_wire(3), Some(Button::Back));
        assert_eq!(Button::from_wire(4), Some(Button::Forward));
        assert_eq!(Button::from_wire(5), None);
    }

    #[test]
    fn normalize_axis_maps_endpoints() {
        // Origin pixel → 0; far edge → 65535; clamped beyond.
        assert_eq!(normalize_axis(0, 0, 1920), 0);
        assert_eq!(normalize_axis(1919, 0, 1920), 65535);
        assert_eq!(normalize_axis(-100, 0, 1920), 0);
        assert_eq!(normalize_axis(9999, 0, 1920), 65535);
        // Negative origin (monitor left of primary): origin maps to 0.
        assert_eq!(normalize_axis(-1920, -1920, 1920), 0);
    }
}
