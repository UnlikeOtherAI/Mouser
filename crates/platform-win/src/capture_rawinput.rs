//! Raw Input (`WM_INPUT`) relative-delta source for ActiveForward capture.
//!
//! While a peer owns input, the `WH_MOUSE_LL` hook (see [`crate::capture_hooks`]) returns
//! `LRESULT(1)` to suppress the OS cursor, which pins it at the screen edge. A
//! successive-absolute delta then decays to zero (the clamped absolute point stops
//! changing), freezing the peer cursor at the entry edge — the same failure macOS had
//! before switching to HID deltas.
//!
//! This module restores true motion: it registers a Raw Input mouse device bound to a
//! message-only (`HWND_MESSAGE`) window on the capture thread and, on `WM_INPUT`, reads
//! the device's relative `lLastX/lLastY` deltas, which keep flowing regardless of where
//! the OS thinks the cursor is. Those deltas drive the peer (`CursorMoved.dx/dy`).
//!
//! Absolute-coordinate devices (tablets, RDP, touch) set `MOUSE_MOVE_ABSOLUTE`; their raw
//! packets carry no usable per-event delta, so they fall back to the hook's
//! absolute-delta path ([`crate::capture_hooks`] notes the device kind so the hook knows
//! whether to enqueue the absolute point).

#![allow(unsafe_code)] // Win32 raw-input FFI; see this crate's Cargo.toml note on unsafe.

use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::{
    GetRawInputData, RegisterRawInputDevices, HRAWINPUT, RAWINPUT, RAWINPUTDEVICE, RAWINPUTHEADER,
    RIDEV_INPUTSINK, RIDEV_REMOVE, RID_INPUT,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, RegisterClassW, HWND_MESSAGE, WINDOW_EX_STYLE,
    WINDOW_STYLE, WM_INPUT, WNDCLASSW,
};

use crate::capture_hooks;

/// `RAWMOUSE.usFlags` bit: the device reports absolute coordinates, not relative deltas.
const MOUSE_MOVE_ABSOLUTE: u16 = 0x0001;
/// `RAWINPUTHEADER.dwType` for a mouse (the `RIM_TYPEMOUSE` constant is 0).
const RIM_TYPE_MOUSE: u32 = 0;
/// HID usage page / usage identifying a generic mouse for Raw Input registration.
const HID_USAGE_PAGE_GENERIC: u16 = 0x0001;
const HID_USAGE_GENERIC_MOUSE: u16 = 0x0002;

const SINK_CLASS_NAME: PCWSTR = w!("MouserRawInputSink");

/// Raw Input source is registered and driving motion via relative deltas; the
/// `WH_MOUSE_LL` hook then only suppresses (unless `RAW_DEVICE_ABSOLUTE`).
static RAW_INPUT_ACTIVE: AtomicBool = AtomicBool::new(false);
/// Most recent raw packet came from an absolute-coordinate device (tablet/RDP/touch), which
/// carries no usable per-event delta, so motion falls back to the hook's absolute path.
static RAW_DEVICE_ABSOLUTE: AtomicBool = AtomicBool::new(false);

/// Mark the Raw Input source active/inactive; clearing also resets the device-kind flag so a
/// later activation does not inherit a stale absolute-device observation.
pub(crate) fn set_raw_input_active(active: bool) {
    RAW_INPUT_ACTIVE.store(active, Ordering::Release);
    if !active {
        RAW_DEVICE_ABSOLUTE.store(false, Ordering::Release);
    }
}

pub(crate) fn raw_input_active() -> bool {
    RAW_INPUT_ACTIVE.load(Ordering::Acquire)
}

pub(crate) fn note_raw_device_absolute(absolute: bool) {
    RAW_DEVICE_ABSOLUTE.store(absolute, Ordering::Release);
}

pub(crate) fn raw_device_absolute() -> bool {
    RAW_DEVICE_ABSOLUTE.load(Ordering::Acquire)
}

/// Decoded motion from a `RAWMOUSE` packet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RawMouseMotion {
    /// True relative device delta — keeps driving the peer even when the OS cursor is
    /// pinned at the screen edge.
    Relative { dx: i32, dy: i32 },
    /// Absolute-coordinate device (tablet/RDP/touch): no usable per-event delta, so the
    /// hook's absolute-delta path handles motion for these.
    Absolute,
}

/// Decode a `RAWMOUSE` (`usFlags`, `lLastX`, `lLastY`) into [`RawMouseMotion`]. Pure and
/// testable: relative unless `MOUSE_MOVE_ABSOLUTE` is set.
pub(crate) fn raw_mouse_motion(flags: u16, last_x: i32, last_y: i32) -> RawMouseMotion {
    if flags & MOUSE_MOVE_ABSOLUTE != 0 {
        RawMouseMotion::Absolute
    } else {
        RawMouseMotion::Relative {
            dx: last_x,
            dy: last_y,
        }
    }
}

/// Register the message-only window class once (idempotent across capture sessions).
fn ensure_sink_class() -> bool {
    static REGISTERED: OnceLock<bool> = OnceLock::new();
    *REGISTERED.get_or_init(|| {
        let Ok(hinstance) = (unsafe { GetModuleHandleW(None) }) else {
            return false;
        };
        let class = WNDCLASSW {
            lpfnWndProc: Some(sink_wndproc),
            hInstance: hinstance.into(),
            lpszClassName: SINK_CLASS_NAME,
            ..Default::default()
        };
        unsafe { RegisterClassW(&class) != 0 }
    })
}

/// Create the hidden message-only window that receives `WM_INPUT`. Returns `None` on
/// failure; the caller then leaves raw input inactive and uses the absolute-delta path.
pub(crate) fn create_sink_window() -> Option<HWND> {
    if !ensure_sink_class() {
        return None;
    }
    let hwnd = unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE(0),
            SINK_CLASS_NAME,
            w!("mouser-rawinput"),
            WINDOW_STYLE(0),
            0,
            0,
            0,
            0,
            Some(HWND_MESSAGE),
            None,
            None,
            None,
        )
    };
    match hwnd {
        Ok(hwnd) if !hwnd.is_invalid() => Some(hwnd),
        _ => None,
    }
}

/// Bind the mouse to `hwnd` with `RIDEV_INPUTSINK` so `WM_INPUT` arrives even when the
/// window is not foreground. Returns `false` on failure.
pub(crate) fn register_raw_mouse(hwnd: HWND) -> bool {
    let device = RAWINPUTDEVICE {
        usUsagePage: HID_USAGE_PAGE_GENERIC,
        usUsage: HID_USAGE_GENERIC_MOUSE,
        dwFlags: RIDEV_INPUTSINK,
        hwndTarget: hwnd,
    };
    let size = std::mem::size_of::<RAWINPUTDEVICE>() as u32;
    unsafe { RegisterRawInputDevices(&[device], size).is_ok() }
}

/// Unbind the mouse Raw Input registration (`RIDEV_REMOVE` requires a null target).
pub(crate) fn unregister_raw_mouse() {
    let device = RAWINPUTDEVICE {
        usUsagePage: HID_USAGE_PAGE_GENERIC,
        usUsage: HID_USAGE_GENERIC_MOUSE,
        dwFlags: RIDEV_REMOVE,
        hwndTarget: HWND(std::ptr::null_mut()),
    };
    let size = std::mem::size_of::<RAWINPUTDEVICE>() as u32;
    let _ = unsafe { RegisterRawInputDevices(&[device], size) };
}

/// Destroy the message-only window created by [`create_sink_window`].
pub(crate) fn destroy_sink_window(hwnd: HWND) {
    let _ = unsafe { DestroyWindow(hwnd) };
}

unsafe extern "system" fn sink_wndproc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if msg == WM_INPUT {
        read_raw_mouse(lparam);
    }
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

/// Read the `WM_INPUT` payload and forward relative deltas to the capture queue. Runs on
/// the capture thread (single dispatcher), so it does not race the worker's state.
fn read_raw_mouse(lparam: LPARAM) {
    let handle = HRAWINPUT(lparam.0 as *mut c_void);
    // RAWINPUT contains a union; zeroed is a valid, inert initial value before the OS fills it.
    let mut raw: RAWINPUT = unsafe { std::mem::zeroed() };
    let mut size = std::mem::size_of::<RAWINPUT>() as u32;
    let header = std::mem::size_of::<RAWINPUTHEADER>() as u32;
    let copied = unsafe {
        GetRawInputData(
            handle,
            RID_INPUT,
            Some((&mut raw as *mut RAWINPUT).cast::<c_void>()),
            &mut size,
            header,
        )
    };
    if copied == 0 || copied == u32::MAX {
        return;
    }
    if raw.header.dwType != RIM_TYPE_MOUSE {
        return;
    }
    // SAFETY: `dwType == RIM_TYPEMOUSE`, so the `mouse` arm of the union is the live one.
    let mouse = unsafe { raw.data.mouse };
    // `usFlags` is the `MOUSE_STATE` newtype in the windows crate; `.0` is the raw `u16`.
    match raw_mouse_motion(mouse.usFlags.0, mouse.lLastX, mouse.lLastY) {
        RawMouseMotion::Relative { dx, dy } => {
            note_raw_device_absolute(false);
            if dx != 0 || dy != 0 {
                capture_hooks::enqueue_raw_mouse_delta(dx, dy);
            }
        }
        RawMouseMotion::Absolute => note_raw_device_absolute(true),
    }
}
