//! macOS input **capture** via `CGEventTap` — SPIKE STUB.
//!
//! This module sketches the capture half of the macOS adapter. It is a *stub*
//! on purpose: a real, asserting capture test cannot run unattended in this
//! environment because of TCC (see "Permissions" below). The code here compiles
//! and installs a tap; whether it receives events depends on grants that a
//! headless build agent cannot self-approve.
//!
//! ## Permissions (TCC) — honest status
//! A session-level `CGEventTap` (the kind that can *observe* and optionally
//! *suppress* global input) requires the host process to hold **both**:
//! - **Accessibility** (Privacy & Security → Accessibility), and
//! - **Input Monitoring** (Privacy & Security → Input Monitoring).
//!
//! Without them `CGEventTapCreate` returns NULL (the `core-graphics`
//! `CGEventTap::new` then yields `Err(())`) — *or* returns a tap that is created
//! but immediately disabled and never fires. There is **no API to grant these
//! from code**; the user must approve them in System Settings, which restarts
//! the process's TCC attribution. A build/CI agent therefore cannot prove
//! capture end-to-end — this is a known, documented limitation, not a code bug.
//!
//! ## Secure Event Input
//! Even with both grants, when a foreground app enables **Secure Event Input**
//! (password fields, some terminals), key events are withheld from taps. The
//! adapter must surface this as `CapState::SecureContext` /
//! `BlockedReason::SecureInputField` (§7.4) and return ownership to the source.
//!
//! ## Lock screen
//! At the macOS lock screen, taps stop delivering — capture is local-only there.

use core_graphics::event::{
    CGEvent, CGEventTap, CGEventTapLocation, CGEventTapOptions, CGEventTapPlacement, CGEventType,
    CallbackResult,
};

/// Why a capture attempt could not proceed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CaptureError {
    /// `CGEventTapCreate` returned NULL — almost always missing Accessibility
    /// and/or Input Monitoring (TCC). Grant both in System Settings, then
    /// relaunch. See module docs.
    TapCreationFailed,
}

impl std::fmt::Display for CaptureError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TapCreationFailed => write!(
                f,
                "CGEventTapCreate returned NULL — grant Accessibility + Input \
                 Monitoring (TCC) to this process and relaunch"
            ),
        }
    }
}

impl std::error::Error for CaptureError {}

/// The default set of event types a capture tap would observe (mouse + keys +
/// scroll). Used by [`install_listen_only_tap`].
#[must_use]
pub fn default_events_of_interest() -> Vec<CGEventType> {
    vec![
        CGEventType::MouseMoved,
        CGEventType::LeftMouseDown,
        CGEventType::LeftMouseUp,
        CGEventType::RightMouseDown,
        CGEventType::RightMouseUp,
        CGEventType::KeyDown,
        CGEventType::KeyUp,
        CGEventType::FlagsChanged,
        CGEventType::ScrollWheel,
    ]
}

/// Install a **listen-only** session tap and run it with `with_fn`.
///
/// `on_event` is invoked for each captured event; the event is always passed
/// through (`ListenOnly` means suppression is ignored anyway). The tap is
/// torn down when `with_fn` returns. `with_fn` must drive the current thread's
/// run loop (e.g. `CFRunLoop::run_current()`) for events to flow.
///
/// Returns `Err(CaptureError::TapCreationFailed)` when the tap can't be created
/// — overwhelmingly a missing-TCC-grant situation (see module docs).
pub fn install_listen_only_tap<R>(
    on_event: impl Fn(CGEventType, &CGEvent) + 'static,
    with_fn: impl FnOnce() -> R,
) -> Result<R, CaptureError> {
    CGEventTap::with_enabled(
        CGEventTapLocation::Session,
        CGEventTapPlacement::HeadInsertEventTap,
        CGEventTapOptions::ListenOnly,
        default_events_of_interest(),
        move |_proxy, etype, event| {
            on_event(etype, event);
            CallbackResult::Keep
        },
        with_fn,
    )
    .map_err(|()| CaptureError::TapCreationFailed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_events_cover_keys_and_mouse() {
        let evs = default_events_of_interest();
        assert!(evs.iter().any(|e| matches!(e, CGEventType::KeyDown)));
        assert!(evs.iter().any(|e| matches!(e, CGEventType::MouseMoved)));
        assert!(evs.iter().any(|e| matches!(e, CGEventType::ScrollWheel)));
    }
}
