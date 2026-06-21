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

use core_graphics::display::CGDisplay;
use core_graphics::event::{
    CGEvent, CGEventTapLocation, CGEventType, CGKeyCode, CGMouseButton, ScrollEventUnit,
};
use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};
use core_graphics::geometry::CGPoint;

use crate::keymap::hid_usage_to_cgkeycode;

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
}

impl std::fmt::Display for InjectError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EventSource => write!(f, "failed to create CGEventSource"),
            Self::EventCreate => write!(f, "failed to create CGEvent"),
            Self::Warp(code) => write!(f, "CGWarpMouseCursorPosition failed (CGError {code})"),
            Self::UnmappedKey(u) => write!(f, "HID usage {u:#06x} has no CGKeyCode mapping"),
        }
    }
}

impl std::error::Error for InjectError {}

fn event_source() -> Result<CGEventSource, InjectError> {
    // HIDSystemState behaves like a real HID device for the window server.
    CGEventSource::new(CGEventSourceStateID::HIDSystemState).map_err(|()| InjectError::EventSource)
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
    let source = CGEventSource::new(CGEventSourceStateID::CombinedSessionState).ok()?;
    let event = CGEvent::new(source).ok()?;
    Some(event.location())
}

/// Move the cursor to a global display point.
///
/// Performs **both** a warp (`CGWarpMouseCursorPosition`, no permission needed,
/// guarantees the cursor relocates) and a posted `MouseMoved` event (needs
/// Accessibility; makes apps see real motion). Warp errors are returned; a
/// dropped `MouseMoved` post is silent (see module docs).
pub fn move_cursor(x: f64, y: f64) -> Result<(), InjectError> {
    let point = CGPoint::new(x, y);
    CGDisplay::warp_mouse_cursor_position(point).map_err(InjectError::Warp)?;

    let source = event_source()?;
    let moved = CGEvent::new_mouse_event(
        source,
        CGEventType::MouseMoved,
        point,
        CGMouseButton::Left,
    )
    .map_err(|()| InjectError::EventCreate)?;
    moved.post(CGEventTapLocation::HID);
    Ok(())
}

/// Synthesize a left-button click (down then up) at a global display point.
pub fn left_click(x: f64, y: f64) -> Result<(), InjectError> {
    let point = CGPoint::new(x, y);
    for ty in [CGEventType::LeftMouseDown, CGEventType::LeftMouseUp] {
        let source = event_source()?;
        let ev = CGEvent::new_mouse_event(source, ty, point, CGMouseButton::Left)
            .map_err(|()| InjectError::EventCreate)?;
        ev.post(CGEventTapLocation::HID);
    }
    Ok(())
}

/// Press or release a key.
///
/// `key` is interpreted as a **HID usage (Usage Page 0x07)** first; if it has no
/// mapping but fits in a `CGKeyCode` it is treated as a raw `CGKeyCode` so
/// callers already holding a virtual key code can pass it through. `down` =
/// true → key-down, false → key-up.
pub fn key_press(key: u16, down: bool) -> Result<(), InjectError> {
    let keycode: CGKeyCode = match hid_usage_to_cgkeycode(key) {
        Some(code) => code,
        // Heuristic for the spike: HID usages live in 0x04..=0xE7; anything
        // outside that range is assumed to already be a raw CGKeyCode.
        None if !(0x04..=0xE7).contains(&key) => key,
        None => return Err(InjectError::UnmappedKey(key)),
    };
    let source = event_source()?;
    let ev = CGEvent::new_keyboard_event(source, keycode, down)
        .map_err(|()| InjectError::EventCreate)?;
    ev.post(CGEventTapLocation::HID);
    Ok(())
}

/// Scroll by pixel deltas (`dx` horizontal, `dy` vertical).
///
/// Wire `Scroll` carries `dx/dy` in either `Detent120` or `LogicalPixel` units
/// (§7.5); the spike injects pixel-unit scrolling. CG axis-1 is vertical,
/// axis-2 is horizontal, so we map `dy`→wheel1, `dx`→wheel2.
pub fn scroll(dx: i32, dy: i32) -> Result<(), InjectError> {
    let source = event_source()?;
    let ev = CGEvent::new_scroll_event(source, ScrollEventUnit::PIXEL, 2, dy, dx, 0)
        .map_err(|()| InjectError::EventCreate)?;
    ev.post(CGEventTapLocation::HID);
    Ok(())
}
