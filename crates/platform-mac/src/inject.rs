//! macOS input **injection** via Core Graphics (`CGEvent` / `CGWarpMouseCursorPosition`).
//!
//! All coordinates are **global display points**, origin top-left, y-down — the
//! same convention as the wire protocol's absolute `PointerMotion` (logical
//! pixels in the target display's space, §7.6). On Retina displays a CG "point"
//! is a logical pixel, so the mapping is direct for the spike.
//!
//! ## TCC permissions
//! Injecting synthetic events requires the host process to hold the
//! **Accessibility** permission (System Settings → Privacy & Security →
//! Accessibility). Without it `CGEventPost` is silently dropped by the window
//! server (no error code is returned) — the cursor will not move and clicks
//! land nowhere. The example binary detects this by reading the cursor position
//! before/after and asserting it changed.
//!
//! `CGWarpMouseCursorPosition` (the warp path used by [`move_cursor`]) does
//! **not** require Accessibility and works without any grant, but it does not
//! generate a real mouse-moved event (apps tracking the cursor won't update).
//! We therefore warp *and* post a `MouseMoved` event so both paths are
//! exercised; the warp guarantees observable motion even when injection is
//! blocked.

use std::cell::RefCell;

use core_graphics::display::{CGDisplay, CGDisplayHideCursor, CGDisplayShowCursor};
use core_graphics::event::{
    CGEvent, CGEventFlags, CGEventTapLocation, CGEventType, CGKeyCode, CGMouseButton, EventField,
    ScrollEventUnit,
};
use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};
use core_graphics::geometry::CGPoint;

use crate::keymap::{hid_usage_to_cgkeycode, mods_to_cgflags};

thread_local! {
    static HID_EVENT_SOURCE: RefCell<Option<CGEventSource>> = const { RefCell::new(None) };
    static COMBINED_EVENT_SOURCE: RefCell<Option<CGEventSource>> = const { RefCell::new(None) };
}

/// Errors from injection calls.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InjectError {
    /// Could not create a `CGEventSource` (rare; resource exhaustion).
    EventSource,
    /// Could not create the `CGEvent` (mouse/keyboard/scroll).
    EventCreate,
    /// `CGWarpMouseCursorPosition` returned a non-success `CGError`.
    Warp(i32),
    /// The HID usage has no macOS key-code mapping yet.
    UnmappedKey(u16),
    /// A pointer button index that is not defined (§7.5 defines 0..=4).
    UnknownButton(u8),
    /// Showing or hiding the cursor returned a non-success `CGError`.
    CursorVisibility(i32),
}

impl std::fmt::Display for InjectError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EventSource => write!(f, "failed to create CGEventSource"),
            Self::EventCreate => write!(f, "failed to create CGEvent"),
            Self::Warp(code) => write!(f, "CGWarpMouseCursorPosition failed (CGError {code})"),
            Self::UnmappedKey(u) => write!(f, "HID usage {u:#06x} has no CGKeyCode mapping"),
            Self::UnknownButton(b) => write!(f, "pointer button index {b} is not defined (§7.5)"),
            Self::CursorVisibility(code) => {
                write!(f, "cursor visibility update failed (CGError {code})")
            }
        }
    }
}

impl std::error::Error for InjectError {}

fn hid_event_source() -> Result<CGEventSource, InjectError> {
    // HIDSystemState behaves like a real HID device for the window server.
    HID_EVENT_SOURCE.with(|cell| {
        if let Some(source) = cell.borrow().as_ref().cloned() {
            return Ok(source);
        }
        let source = CGEventSource::new(CGEventSourceStateID::HIDSystemState)
            .map_err(|()| InjectError::EventSource)?;
        *cell.borrow_mut() = Some(source.clone());
        Ok(source)
    })
}

fn combined_event_source() -> Option<CGEventSource> {
    COMBINED_EVENT_SOURCE.with(|cell| {
        if let Some(source) = cell.borrow().as_ref().cloned() {
            return Some(source);
        }
        let source = CGEventSource::new(CGEventSourceStateID::CombinedSessionState).ok()?;
        *cell.borrow_mut() = Some(source.clone());
        Some(source)
    })
}

/// Read the current global cursor position via Core Graphics.
///
/// Uses `CGEventGetLocation(CGEventCreate(NULL))` — a NULL-source event carries
/// the live cursor location. This is the ground-truth oracle the demo uses to
/// prove the cursor actually moved.
#[must_use]
pub fn cursor_position() -> Option<CGPoint> {
    // `CGEvent::new` with a freshly created default source mirrors
    // `CGEventCreate(NULL)`; the resulting event's location is the cursor.
    let source = combined_event_source()?;
    let event = CGEvent::new(source).ok()?;
    Some(event.location())
}

/// Move the cursor to a **global** display point.
///
/// Performs **both** a warp (`CGWarpMouseCursorPosition`, no permission needed,
/// guarantees the cursor relocates) and a posted `MouseMoved` event (needs
/// Accessibility; makes apps see real motion). Warp errors are returned; a
/// dropped `MouseMoved` post is silent (see module docs).
///
/// Callers holding display-local coordinates translate to a global point first
/// (`display_info::DisplayBounds::local_to_global`) — that is what
/// [`crate::adapter::MacInjector::move_cursor`] does (audit M1).
pub fn move_cursor(x: f64, y: f64) -> Result<(), InjectError> {
    let point = CGPoint::new(x, y);
    warp_cursor(x, y)?;

    let source = hid_event_source()?;
    let moved =
        CGEvent::new_mouse_event(source, CGEventType::MouseMoved, point, CGMouseButton::Left)
            .map_err(|()| InjectError::EventCreate)?;
    moved.post(CGEventTapLocation::HID);
    Ok(())
}

/// Move the cursor to a global display point without posting a mouse-moved event.
///
/// Used only for local cursor parking/restoration during remote ownership. Posting a
/// synthetic move there can be captured by Mouser itself and forwarded as a bogus peer
/// delta, so the parking path must be warp-only.
pub fn warp_cursor(x: f64, y: f64) -> Result<(), InjectError> {
    let point = CGPoint::new(x, y);
    CGDisplay::warp_mouse_cursor_position(point).map_err(InjectError::Warp)
}

/// Apply a **relative** cursor delta in points (spec §7.6 tag 0x02), used when
/// the foreground app has grabbed pointer-lock.
///
/// Reads the current cursor location, offsets it, and posts a `MouseMoved` event
/// carrying `(dx, dy)` in the delta fields so relative consumers see motion even
/// when the absolute position is ignored.
pub fn move_cursor_rel(dx: i32, dy: i32) -> Result<(), InjectError> {
    let current = cursor_position().unwrap_or_else(|| CGPoint::new(0.0, 0.0));
    let point = CGPoint::new(current.x + f64::from(dx), current.y + f64::from(dy));
    let source = hid_event_source()?;
    let ev = CGEvent::new_mouse_event(source, CGEventType::MouseMoved, point, CGMouseButton::Left)
        .map_err(|()| InjectError::EventCreate)?;
    ev.set_integer_value_field(EventField::MOUSE_EVENT_DELTA_X, i64::from(dx));
    ev.set_integer_value_field(EventField::MOUSE_EVENT_DELTA_Y, i64::from(dy));
    ev.post(CGEventTapLocation::HID);
    Ok(())
}

/// Press or release a pointer button by §7.5 index (0=left,1=right,2=middle,
/// 3=back,4=forward) at the **current** cursor position.
///
/// Left/right/middle use the dedicated `CGMouseButton`s; back/forward post
/// `OtherMouse*` with the button number set. Returns
/// [`InjectError::UnknownButton`] for any other index.
pub fn button(index: u8, down: bool) -> Result<(), InjectError> {
    let point = cursor_position().unwrap_or_else(|| CGPoint::new(0.0, 0.0));
    let (ty, cg_button, number) = match (index, down) {
        (0, true) => (CGEventType::LeftMouseDown, CGMouseButton::Left, 0),
        (0, false) => (CGEventType::LeftMouseUp, CGMouseButton::Left, 0),
        (1, true) => (CGEventType::RightMouseDown, CGMouseButton::Right, 1),
        (1, false) => (CGEventType::RightMouseUp, CGMouseButton::Right, 1),
        (2, true) => (CGEventType::OtherMouseDown, CGMouseButton::Center, 2),
        (2, false) => (CGEventType::OtherMouseUp, CGMouseButton::Center, 2),
        (3, true) => (CGEventType::OtherMouseDown, CGMouseButton::Center, 3),
        (3, false) => (CGEventType::OtherMouseUp, CGMouseButton::Center, 3),
        (4, true) => (CGEventType::OtherMouseDown, CGMouseButton::Center, 4),
        (4, false) => (CGEventType::OtherMouseUp, CGMouseButton::Center, 4),
        (other, _) => return Err(InjectError::UnknownButton(other)),
    };
    let source = hid_event_source()?;
    let ev = CGEvent::new_mouse_event(source, ty, point, cg_button)
        .map_err(|()| InjectError::EventCreate)?;
    ev.set_integer_value_field(EventField::MOUSE_EVENT_BUTTON_NUMBER, number);
    ev.post(CGEventTapLocation::HID);
    Ok(())
}

/// Synthesize a left-button click (down then up) at a global display point.
///
/// Builds **both** events before posting either, so a creation failure can't
/// leave the button logically held (audit M3).
pub fn left_click(x: f64, y: f64) -> Result<(), InjectError> {
    let point = CGPoint::new(x, y);
    let mut events = Vec::with_capacity(2);
    for ty in [CGEventType::LeftMouseDown, CGEventType::LeftMouseUp] {
        let source = hid_event_source()?;
        let ev = CGEvent::new_mouse_event(source, ty, point, CGMouseButton::Left)
            .map_err(|()| InjectError::EventCreate)?;
        events.push(ev);
    }
    for ev in events {
        ev.post(CGEventTapLocation::HID);
    }
    Ok(())
}

/// Press or release a key with active modifiers.
///
/// `key` is interpreted as a **HID usage (Usage Page 0x07)** first; if it has no
/// mapping but fits in a `CGKeyCode` it is treated as a raw `CGKeyCode` so
/// callers already holding a virtual key code can pass it through. `mods` is the
/// wire bitmask (Appendix B); `cmd_ctrl_swap` applies the optional macOS Cmd↔Ctrl
/// swap. `down = true` → key-down, false → key-up.
pub fn key_press(key: u16, down: bool, mods: u16, cmd_ctrl_swap: bool) -> Result<(), InjectError> {
    let keycode: CGKeyCode = match hid_usage_to_cgkeycode(key) {
        Some(code) => code,
        // HID usages live in 0x04..=0xE7; anything outside is assumed to already
        // be a raw CGKeyCode the caller resolved.
        None if !(0x04..=0xE7).contains(&key) => key,
        None => return Err(InjectError::UnmappedKey(key)),
    };
    let source = hid_event_source()?;
    let ev = CGEvent::new_keyboard_event(source, keycode, down)
        .map_err(|()| InjectError::EventCreate)?;
    let flags = mods_to_cgflags(mods, cmd_ctrl_swap);
    if flags != CGEventFlags::CGEventFlagNull {
        ev.set_flags(flags);
    }
    ev.post(CGEventTapLocation::HID);
    Ok(())
}

/// Scroll by deltas (`dx` horizontal, `dy` vertical).
///
/// Wire `Scroll` carries `dx/dy` in either `Detent120` or `LogicalPixel` units
/// (§7.5). `pixel = true` injects pixel-unit (high-resolution/trackpad)
/// scrolling; `false` injects line/detent units. CG axis-1 is vertical, axis-2
/// is horizontal, so `dy`→wheel1, `dx`→wheel2.
pub fn scroll(dx: i32, dy: i32, pixel: bool) -> Result<(), InjectError> {
    let unit = if pixel {
        ScrollEventUnit::PIXEL
    } else {
        ScrollEventUnit::LINE
    };
    let source = hid_event_source()?;
    let ev = CGEvent::new_scroll_event(source, unit, 2, dy, dx, 0)
        .map_err(|()| InjectError::EventCreate)?;
    ev.post(CGEventTapLocation::HID);
    Ok(())
}

/// Show or hide the cursor on all active displays.
///
/// CoreGraphics hide/show is reference-counted per process, so callers must make
/// this idempotent at a higher layer. [`crate::injector::MacInjector`] does that
/// before calling here.
pub fn set_cursor_visible(visible: bool) -> Result<(), InjectError> {
    let displays = CGDisplay::active_displays().unwrap_or_else(|_| vec![CGDisplay::main().id]);
    let mut first_error = None;
    for display in displays {
        // SAFETY: `display` comes from CoreGraphics' active display list, or from
        // `CGMainDisplayID` as a fallback. The call has no Rust aliasing contract.
        let code = unsafe {
            if visible {
                CGDisplayShowCursor(display)
            } else {
                CGDisplayHideCursor(display)
            }
        };
        if code != 0 && first_error.is_none() {
            first_error = Some(code);
        }
    }
    match first_error {
        Some(code) => Err(InjectError::CursorVisibility(code)),
        None => Ok(()),
    }
}
