//! Linux uinput backend for the input-injection spike.
//!
//! Opens `/dev/uinput`, declares a combined pointer + keyboard capability set,
//! and emits relative motion, mouse-button, and key events. The kernel exposes
//! the result as a normal evdev device named [`DEVICE_NAME`], visible under
//! `/proc/bus/input/devices` and `/dev/input/by-id/`.

use std::fs::OpenOptions;
use std::io;
use std::os::unix::fs::OpenOptionsExt;

use input_linux::{
    EventKind, EventTime, InputId, Key, KeyEvent, KeyState, RelativeAxis, RelativeEvent,
    SynchronizeEvent, SynchronizeKind, UInputHandle,
};

/// Name the kernel advertises for our virtual device.
pub const DEVICE_NAME: &str = "mouser-virtual";

/// Path to the uinput control node.
const UINPUT_PATH: &str = "/dev/uinput";

/// USB bus type (`BUS_USB`), used purely cosmetically for the device id.
const BUS_USB: u16 = 0x03;

/// Mouse buttons this spike can synthesize.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Button {
    /// Primary (left) mouse button — `BTN_LEFT`.
    Left,
    /// Secondary (right) mouse button — `BTN_RIGHT`.
    Right,
    /// Middle / wheel mouse button — `BTN_MIDDLE`.
    Middle,
}

impl Button {
    fn key(self) -> Key {
        match self {
            Button::Left => Key::ButtonLeft,
            Button::Right => Key::ButtonRight,
            Button::Middle => Key::ButtonMiddle,
        }
    }
}

/// A virtual mouse + keyboard backed by `/dev/uinput`.
///
/// Dropping the value tears the kernel device down (best-effort
/// `UI_DEV_DESTROY`), so the evdev node disappears when the handle goes away.
pub struct VirtualDevice {
    handle: UInputHandle<std::fs::File>,
}

impl VirtualDevice {
    /// Open `/dev/uinput`, register pointer + keyboard capabilities, and ask
    /// the kernel to materialize the device.
    ///
    /// # Errors
    /// Returns the underlying [`io::Error`] if `/dev/uinput` cannot be opened
    /// (commonly `EACCES` — needs the `input` group or root) or if any uinput
    /// ioctl fails.
    pub fn new() -> io::Result<Self> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc_nonblock())
            .open(UINPUT_PATH)?;
        let handle = UInputHandle::new(file);

        // Declare which event classes this device produces.
        handle.set_evbit(EventKind::Key)?;
        handle.set_evbit(EventKind::Relative)?;
        handle.set_evbit(EventKind::Synchronize)?;

        // Relative pointer axes.
        handle.set_relbit(RelativeAxis::X)?;
        handle.set_relbit(RelativeAxis::Y)?;

        // Mouse buttons.
        for button in [Button::Left, Button::Right, Button::Middle] {
            handle.set_keybit(button.key())?;
        }

        // A handful of representative keyboard keys for the spike.
        for key in spike_keys() {
            handle.set_keybit(key)?;
        }

        let id = InputId {
            bustype: BUS_USB,
            vendor: 0x1d6b,  // "Linux Foundation" vendor id, cosmetic.
            product: 0x4d53, // "MS" — mouser, cosmetic.
            version: 1,
        };

        handle.create(&id, DEVICE_NAME.as_bytes(), 0, &[])?;
        Ok(Self { handle })
    }

    /// Emit a relative pointer move by `(dx, dy)` device units.
    ///
    /// # Errors
    /// Propagates any write error from the uinput fd.
    pub fn move_rel(&self, dx: i32, dy: i32) -> io::Result<()> {
        let t = now();
        let events = [
            *RelativeEvent::new(t, RelativeAxis::X, dx).as_event(),
            *RelativeEvent::new(t, RelativeAxis::Y, dy).as_event(),
            *sync(t).as_event(),
        ];
        self.write_all(&events)
    }

    /// Press (`down = true`) or release (`down = false`) a mouse button.
    ///
    /// # Errors
    /// Propagates any write error from the uinput fd.
    pub fn button(&self, button: Button, down: bool) -> io::Result<()> {
        self.emit_key(button.key(), down)
    }

    /// Press (`down = true`) or release (`down = false`) a keyboard key.
    ///
    /// # Errors
    /// Propagates any write error from the uinput fd.
    pub fn key(&self, key: Key, down: bool) -> io::Result<()> {
        self.emit_key(key, down)
    }

    /// Filesystem path of the created device under `/sys`, if the kernel
    /// reports one. Useful for test evidence.
    ///
    /// # Errors
    /// Propagates the ioctl error.
    pub fn sys_path(&self) -> io::Result<std::path::PathBuf> {
        self.handle.sys_path()
    }

    fn emit_key(&self, key: Key, down: bool) -> io::Result<()> {
        let t = now();
        let state = if down {
            KeyState::PRESSED
        } else {
            KeyState::RELEASED
        };
        let events = [
            *KeyEvent::new(t, key, state).as_event(),
            *sync(t).as_event(),
        ];
        self.write_all(&events)
    }

    fn write_all(&self, events: &[input_linux::InputEvent]) -> io::Result<()> {
        // `input-linux` writes raw `input_event`s; `InputEvent` is layout-compatible.
        let raw: Vec<_> = events.iter().map(|e| *e.as_raw()).collect();
        let written = self.handle.write(&raw)?;
        if written != raw.len() {
            return Err(io::Error::new(
                io::ErrorKind::WriteZero,
                "short write to /dev/uinput",
            ));
        }
        Ok(())
    }
}

impl Drop for VirtualDevice {
    fn drop(&mut self) {
        // Best-effort teardown; nothing useful to do on error during drop.
        let _ = self.handle.dev_destroy();
    }
}

/// A representative slice of keyboard keys enabled on the spike device.
fn spike_keys() -> [Key; 5] {
    [Key::A, Key::B, Key::C, Key::Enter, Key::Space]
}

/// Build a `SYN_REPORT` event closing an input report.
fn sync(t: EventTime) -> SynchronizeEvent {
    SynchronizeEvent::new(t, SynchronizeKind::Report, 0)
}

/// uinput accepts a zero timestamp; the kernel stamps events itself.
fn now() -> EventTime {
    EventTime::new(0, 0)
}

/// `O_NONBLOCK` — open the control node non-blocking so reads never stall.
fn libc_nonblock() -> i32 {
    0o0004000
}
