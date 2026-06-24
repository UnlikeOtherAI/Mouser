//! `LinuxCapture` — the Linux [`InputCapture`] adapter (audit H3), the source-side
//! counterpart to [`crate::adapter::UinputInjector`].
//!
//! It enumerates `/dev/input/event*`, opens the keyboard + pointer devices, reads
//! evdev events on a background thread, translates them into the wire's
//! [`LocalInputEvent`] model (HID usages on Usage Page 0x07, §7.5 button indices),
//! and hands each to the [`InputSink`]. When the sink returns
//! [`CaptureDecision::Suppress`] the adapter `EVIOCGRAB`-grabs the devices so the
//! events no longer reach the local desktop (the machine is forwarding input to a
//! remote peer); on [`CaptureDecision::PassThrough`] it ungrabs. Grabbing is the
//! Linux suppression primitive, so [`InputCapture::can_suppress`] is `true` once
//! at least one device is open.
//!
//! ## Suppression granularity (vs macOS)
//! Unlike the macOS `CGEventTap` — which sits inline and can `Drop` the *very*
//! event the sink rejected — evdev hands the reader a *copy* of each event; a
//! grab only stops *future* delivery to the desktop. So the first event that
//! flips the engine into "suppress" still reaches the local desktop, and the
//! grab takes effect from the next event onward. For sustained ownership (the
//! steady state of a handoff) the devices stay grabbed and local input is fully
//! swallowed; only the single boundary event leaks. This is inherent to the
//! evdev model and acceptable for the active-device handoff (the boundary event
//! is what crosses the edge in the first place).
//!
//! Linux-only: it needs `/dev/input/event*` and `input_linux::EvdevHandle`.
//!
//! ## Coordinate model
//! evdev pointers report **relative** motion (`REL_X`/`REL_Y`), while Mouser's
//! wire model uses display-local absolute positions. On X11 this adapter resolves
//! the real cursor with XQueryPointer and maps it through RandR output bounds.
//! While actively suppressing with `EVIOCGRAB`, Xorg may stop receiving the grabbed
//! device, so motion integrates from the last real X11 point until pass-through
//! resumes. Wayland absolute cursor parity needs a compositor-specific backend.

use std::fs::{File, OpenOptions};
use std::os::unix::fs::OpenOptionsExt;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use input_linux::{EvdevHandle, Key};
use mouser_core::platform::{
    CaptureDecision, CaptureMode, InputCapture, InputSink, LocalInputEvent, PlatformError,
    PlatformResult,
};

use crate::capture_translate::{to_local_event, CursorMapper};

/// Directory the kernel exposes evdev device nodes under.
const INPUT_DIR: &str = "/dev/input";

/// `O_NONBLOCK` — opened non-blocking so the read loop can poll a stop flag
/// instead of parking forever in `read` (mirrors `uinput::libc_nonblock`).
fn o_nonblock() -> i32 {
    0o0004000
}

/// How long the read loop sleeps between empty (`WouldBlock`) polls. Short enough
/// that `stop()` is observed promptly; the cost is only paid when idle (real
/// events return immediately).
const IDLE_POLL: Duration = Duration::from_millis(4);

const LEFT_CTRL_BIT: u16 = 1 << 0;
const RIGHT_CTRL_BIT: u16 = 1 << 4;
const EMERGENCY_RECLAIM_CTRL_MASK: u16 = LEFT_CTRL_BIT | RIGHT_CTRL_BIT;

/// The local emergency-reclaim chord is both Ctrl keys held at once.
#[must_use]
pub fn emergency_reclaim_chord_from_mods(mods: u16) -> bool {
    mods & EMERGENCY_RECLAIM_CTRL_MASK == EMERGENCY_RECLAIM_CTRL_MASK
}

/// Whether a captured key event exposes the emergency-reclaim chord to the sink.
#[must_use]
pub fn is_emergency_reclaim_event(event: LocalInputEvent) -> bool {
    matches!(
        event,
        LocalInputEvent::Key {
            down: true,
            mods,
            ..
        } if emergency_reclaim_chord_from_mods(mods)
    )
}

/// One opened evdev device the capture reads from and (when suppressing) grabs.
struct CaptureDevice {
    /// Path of the node, for diagnostics.
    path: PathBuf,
    /// The evdev handle (owns the open fd).
    handle: EvdevHandle<File>,
    /// Whether this device is currently `EVIOCGRAB`-grabbed.
    grabbed: bool,
}

/// Mutable run state shared between [`LinuxCapture`] and its reader thread.
struct CaptureRun {
    /// Set to `false` by [`LinuxCapture::stop`] to ask the reader thread to exit.
    running: Arc<AtomicBool>,
    /// Join handle for the reader thread, so `stop` can wait for it to drain.
    thread: Option<std::thread::JoinHandle<()>>,
    /// Whether suppression is possible right now (devices open AND actively
    /// forwarding). Reading evdev without `EVIOCGRAB` is non-intrusive, so the
    /// passive-edge mode keeps the reader running but never grabs.
    can_suppress: bool,
    /// The capture mode the adapter is currently in.
    mode: CaptureMode,
}

/// Linux input capture over the raw evdev devices (audit H3).
pub struct LinuxCapture {
    inner: Arc<Mutex<CaptureRun>>,
}

impl Default for LinuxCapture {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for LinuxCapture {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

impl LinuxCapture {
    /// A not-yet-started capture handle.
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(CaptureRun {
                running: Arc::new(AtomicBool::new(false)),
                thread: None,
                can_suppress: false,
                mode: CaptureMode::Off,
            })),
        }
    }
}

/// Lock a capture mutex on a runtime path without ever panicking.
///
/// The guarded [`CaptureRun`] carries no broken invariant after a panic (it is a
/// thread handle + a couple of flags), so recovering the poisoned guard via
/// [`PoisonError::into_inner`] is correct and keeps capture from aborting — the
/// same discipline `platform-mac` and the uinput injector use.
fn lock_recover<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

#[derive(Debug, Default)]
struct EmergencyReclaim {
    held_mods: u16,
    active: bool,
}

impl EmergencyReclaim {
    fn new() -> Self {
        Self::default()
    }

    fn observe(&mut self, event: LocalInputEvent) -> (LocalInputEvent, bool) {
        let was_active = self.active;
        let event = match event {
            LocalInputEvent::Key { usage, down, .. } => {
                self.update_modifier(usage, down);
                if emergency_reclaim_chord_from_mods(self.held_mods) {
                    self.active = true;
                }
                LocalInputEvent::Key {
                    usage,
                    down,
                    mods: self.held_mods,
                }
            }
            other => other,
        };
        let force_pass = was_active || self.active;
        if self.active && self.held_mods & EMERGENCY_RECLAIM_CTRL_MASK == 0 {
            self.active = false;
        }
        (event, force_pass)
    }

    fn update_modifier(&mut self, usage: u16, down: bool) {
        let Some(bit) = modifier_bit(usage) else {
            return;
        };
        if down {
            self.held_mods |= bit;
        } else {
            self.held_mods &= !bit;
        }
    }
}

fn modifier_bit(usage: u16) -> Option<u16> {
    if (0xE0..=0xE7).contains(&usage) {
        Some(1 << (usage - 0xE0))
    } else {
        None
    }
}

fn decision_for(
    sink: &dyn InputSink,
    event: LocalInputEvent,
    reclaim: &mut EmergencyReclaim,
) -> CaptureDecision {
    let (event, force_pass) = reclaim.observe(event);
    let decision = catch_unwind(AssertUnwindSafe(|| sink.on_event(event)))
        .unwrap_or(CaptureDecision::PassThrough);
    if force_pass {
        CaptureDecision::PassThrough
    } else {
        decision
    }
}

/// Capture could not open any usable input device — almost always missing
/// permission on `/dev/input/event*` (needs the `input` group or root).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NoCaptureDevices;

impl std::fmt::Display for NoCaptureDevices {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "no readable /dev/input/event* devices — add the user to the `input` \
             group (or run as root) and relaunch"
        )
    }
}

impl std::error::Error for NoCaptureDevices {}

/// Decide whether an opened device is one we want to read (a keyboard or a
/// pointer). A device qualifies if it reports relative X+Y axes (a pointer) or
/// any of the canonical keyboard keys (a keyboard). Devices we can't query are
/// skipped (returns `false`) rather than erroring.
fn is_input_of_interest(handle: &EvdevHandle<File>) -> bool {
    // Pointer: has relative X and Y.
    if let Ok(rel) = handle.relative_bits() {
        if rel.get(input_linux::RelativeAxis::X) && rel.get(input_linux::RelativeAxis::Y) {
            return true;
        }
    }
    // Keyboard: reports a representative letter key (KEY_A) or Esc.
    if let Ok(keys) = handle.key_bits() {
        if keys.get(Key::A) || keys.get(Key::Esc) {
            return true;
        }
    }
    false
}

/// Enumerate `/dev/input/event*`, open each non-blocking, and keep the keyboards
/// and pointers. Unreadable nodes (permission, hot-unplug) are skipped, never
/// fatal — robust enumeration (audit: don't panic on a bad device).
fn open_devices(dir: &Path) -> Vec<CaptureDevice> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return out;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let is_event_node = path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.starts_with("event"));
        if !is_event_node {
            continue;
        }
        let Ok(file) = OpenOptions::new()
            .read(true)
            .custom_flags(o_nonblock())
            .open(&path)
        else {
            continue; // permission / transient — skip, don't fail capture.
        };
        let handle = EvdevHandle::new(file);
        if is_input_of_interest(&handle) {
            out.push(CaptureDevice {
                path,
                handle,
                grabbed: false,
            });
        }
    }
    out
}

/// Apply the sink's [`CaptureDecision`] to the device grab state: grab all
/// devices when suppressing, ungrab when passing through. Grab failures are
/// logged-by-ignoring (best-effort) so a single uncooperative device can't abort
/// capture; the engine still learns suppression may be partial via
/// [`InputCapture::can_suppress`].
fn apply_decision(devices: &mut [CaptureDevice], decision: CaptureDecision) {
    let want_grab = matches!(decision, CaptureDecision::Suppress);
    for dev in devices.iter_mut() {
        if dev.grabbed != want_grab {
            match dev.handle.grab(want_grab) {
                Ok(()) => dev.grabbed = want_grab,
                Err(e) => eprintln!(
                    "mouser-capture: {} EVIOCGRAB failed on {}: {e}",
                    if want_grab { "grab" } else { "ungrab" },
                    dev.path.display()
                ),
            }
        }
    }
}

/// Read one batch of pending events from a single device and feed them to the
/// sink, accumulating the strongest decision (Suppress wins, so a frame that
/// should be swallowed is). Returns the decision for the batch; on `WouldBlock`
/// (no pending events) returns `PassThrough` and `read_any = false`.
fn drain_device(
    dev: &EvdevHandle<File>,
    sink: &dyn InputSink,
    cursor: &CursorMapper,
    reclaim: &mut EmergencyReclaim,
) -> (CaptureDecision, bool) {
    let mut decision = CaptureDecision::PassThrough;
    let mut read_any = false;
    // Drain everything currently buffered, then return to the poll loop. The
    // loop ends on the first `Err` — `WouldBlock` (nothing pending on the
    // non-blocking fd) or a transient read hiccup.
    while let Ok(ev) = dev.read_input_event() {
        read_any = true;
        if let Some(local) = to_local_event(ev.kind, ev.code, ev.value, cursor) {
            let event_decision = decision_for(sink, local, reclaim);
            cursor.set_integrating(matches!(event_decision, CaptureDecision::Suppress));
            if matches!(event_decision, CaptureDecision::Suppress) {
                decision = CaptureDecision::Suppress;
            }
        }
    }
    (decision, read_any)
}

impl LinuxCapture {
    /// Ensure the evdev reader thread is running, opening the keyboard/pointer
    /// devices if needed. Idempotent: a no-op when already running. The reader only
    /// `EVIOCGRAB`s (suppresses) when the sink returns [`CaptureDecision::Suppress`],
    /// which the engine does only while actively forwarding — so this same reader
    /// serves both passive edge sensing and active forwarding.
    fn ensure_running(&self, sink: Arc<dyn InputSink>) -> PlatformResult<()> {
        let mut guard = lock_recover(&self.inner);
        // Idempotent: already capturing.
        if guard.thread.is_some() {
            return Ok(());
        }

        let mut devices = open_devices(Path::new(INPUT_DIR));
        if devices.is_empty() {
            return Err(Box::new(NoCaptureDevices));
        }

        let running = Arc::new(AtomicBool::new(true));
        guard.running = Arc::clone(&running);

        let cursor = CursorMapper::new();

        let handle = std::thread::Builder::new()
            .name("mouser-linux-capture".into())
            .spawn(move || {
                let mut reclaim = EmergencyReclaim::new();
                while running.load(Ordering::Relaxed) {
                    let mut any = false;
                    let mut frame = CaptureDecision::PassThrough;
                    for dev in devices.iter() {
                        let (decision, read_any) =
                            drain_device(&dev.handle, sink.as_ref(), &cursor, &mut reclaim);
                        any |= read_any;
                        if matches!(decision, CaptureDecision::Suppress) {
                            frame = CaptureDecision::Suppress;
                        }
                    }
                    // Honor the engine's swallow-vs-passthrough decision by
                    // grabbing/ungrabbing the devices.
                    apply_decision(&mut devices, frame);
                    if !any {
                        std::thread::sleep(IDLE_POLL);
                    }
                }
                // Exiting: ungrab everything so local input is restored.
                apply_decision(&mut devices, CaptureDecision::PassThrough);
            })
            .map_err(|e| -> PlatformError { Box::new(e) })?;

        guard.thread = Some(handle);
        Ok(())
    }
}

impl InputCapture for LinuxCapture {
    fn set_mode(&self, mode: CaptureMode, sink: &Arc<dyn InputSink>) -> PlatformResult<()> {
        match mode {
            CaptureMode::Off => self.stop(),
            CaptureMode::PassiveEdge | CaptureMode::ActiveForward => {
                // The evdev reader serves both modes (it only grabs when the sink
                // says Suppress, which the engine does only while forwarding). Ensure
                // it is running, then record the mode and whether we can suppress.
                self.ensure_running(Arc::clone(sink))?;
                let mut guard = lock_recover(&self.inner);
                guard.mode = mode;
                guard.can_suppress =
                    matches!(mode, CaptureMode::ActiveForward) && guard.thread.is_some();
                Ok(())
            }
        }
    }

    fn stop(&self) -> PlatformResult<()> {
        let thread = {
            let mut guard = lock_recover(&self.inner);
            guard.running.store(false, Ordering::Relaxed);
            guard.can_suppress = false;
            guard.mode = CaptureMode::Off;
            guard.thread.take()
        };
        if let Some(handle) = thread {
            // The reader thread wakes within `IDLE_POLL` and ungrabs on exit.
            let _ = handle.join();
        }
        Ok(())
    }

    fn can_suppress(&self) -> bool {
        lock_recover(&self.inner).can_suppress
    }

    fn current_mode(&self) -> CaptureMode {
        lock_recover(&self.inner).mode
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_devices_is_a_clear_error() {
        // An empty/nonexistent dir yields no devices; start surfaces the
        // permission-style error rather than panicking.
        let cap = LinuxCapture::new();
        let devices = open_devices(Path::new("/nonexistent-mouser-test-dir"));
        assert!(devices.is_empty());
        // can_suppress is false before start.
        assert!(!cap.can_suppress());
        assert!(cap.stop().is_ok());
    }
}
