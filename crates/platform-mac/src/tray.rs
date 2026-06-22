//! macOS menu-bar tray adapter — `NSStatusItem`-backed [`mouser_core::platform::Tray`].
//!
//! [`MacTray`] owns a single system-status-bar item ([`NSStatusItem`]) and reflects
//! the engine's [`TrayState`] / status line onto its menu-bar button
//! ([`NSStatusBarButton`]). It implements the small `Tray` contract (spec §9 owner
//! indicator): [`Tray::set_status`] writes the button title + tooltip, and
//! [`Tray::set_state`] writes a short state glyph as the button title.
//!
//! ## Main-thread / run-loop requirement (hard AppKit constraint)
//! `NSStatusItem` is a GUI, **main-thread-only**, WindowServer-attached object:
//! - It must be **created and mutated on the main thread** of a process that owns a
//!   running `NSApplication` / `CFRunLoop`. [`MacTray::new`] takes a
//!   [`MainThreadMarker`], which only exists on the main thread, so construction off
//!   the main thread is a compile-/runtime-checked impossibility.
//! - Every mutator ([`set_status`](Tray::set_status) / [`set_state`](Tray::set_state))
//!   re-acquires a [`MainThreadMarker`] via [`MainThreadMarker::new`] and returns
//!   [`TrayError::NotMainThread`] if called off the main thread, rather than risking
//!   undefined behavior by messaging AppKit from a worker.
//! - The item only **appears in the menu bar while a run loop is pumping**. Spinning
//!   that run loop (an `NSApplication.run` / `CFRunLoop`) is the job of the AppKit
//!   *host*; this adapter assumes it is constructed from such a host and does **not**
//!   integrate with the tokio-based `mouserd` daemon. Run-loop integration is a
//!   separate task — `MacTray` is only the adapter.
//!
//! ## `Send + Sync`
//! The `Tray` trait is `Send + Sync`, but `Retained<NSStatusItem>` is neither (AppKit
//! objects are not thread-safe). `MacTray` carries the handles only so the
//! main-thread-gated mutators can reach them; the [`unsafe impl Send for MacTray`] /
//! [`unsafe impl Sync for MacTray`] are sound **because every method that touches an
//! AppKit object first proves it is on the main thread** (`MainThreadMarker::new`),
//! and the handles are never dereferenced off it.
//!
//! ## `unsafe`
//! The AppKit accessors used here (`statusItemWithLength:`, `button:`, `setTitle:`,
//! `setToolTip:`, `removeStatusItem:`) are safe in this `objc2` binding; the only
//! `unsafe` is the two thread-marker `Send`/`Sync` impls, each carrying a `// SAFETY:`
//! note. The rest stays wrapper-only (see `lib.rs`).

use mouser_core::platform::{PlatformError, PlatformResult, Tray, TrayState};
use objc2::rc::Retained;
use objc2::MainThreadMarker;
use objc2_app_kit::{NSStatusBar, NSStatusBarButton, NSStatusItem, NSVariableStatusItemLength};
use objc2_foundation::NSString;

/// A short, glanceable label for a [`TrayState`], shown as the menu-bar button title.
///
/// Pure mapping with no AppKit dependency, so it is unit-testable without a GUI: the
/// glyphs are single Unicode characters chosen to read at menu-bar size — a filled
/// dot when *owning*, a hollow dot when *idle*, a "no entry" when *blocked*, and a
/// dashed circle when *disconnected*.
#[must_use]
pub fn state_label(state: TrayState) -> &'static str {
    match state {
        TrayState::Owning => "\u{25CF}",       // ● BLACK CIRCLE
        TrayState::Idle => "\u{25CB}",         // ○ WHITE CIRCLE
        TrayState::Blocked => "\u{26D4}",      // ⛔ NO ENTRY
        TrayState::Disconnected => "\u{25CC}", // ◌ DOTTED CIRCLE
    }
}

/// A human-readable tooltip describing a [`TrayState`].
///
/// Used as the button's tooltip alongside the [`state_label`] glyph so hovering the
/// menu-bar item reads in words. Pure, so it is unit-testable without AppKit.
#[must_use]
pub fn state_tooltip(state: TrayState) -> &'static str {
    match state {
        TrayState::Owning => "Mouser: owning input",
        TrayState::Idle => "Mouser: connected (idle)",
        TrayState::Blocked => "Mouser: input blocked",
        TrayState::Disconnected => "Mouser: disconnected",
    }
}

/// Why a tray operation could not run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrayError {
    /// Called off the main thread — `NSStatusItem` is main-thread-only (module docs).
    NotMainThread,
    /// The status item has no backing button (it can be `nil` before the menu bar is
    /// up, or for a custom-`view` item); there is nothing to set a title/tooltip on.
    NoButton,
}

impl std::fmt::Display for TrayError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotMainThread => write!(f, "tray must be used on the main thread"),
            Self::NoButton => write!(f, "status item has no button to update"),
        }
    }
}

impl std::error::Error for TrayError {}

/// `NSStatusItem`-backed [`Tray`] over the system status bar.
///
/// Construct on the main thread of an AppKit host (see module docs); the item stays in
/// the menu bar for the lifetime of this value and is removed on [`Drop`].
pub struct MacTray {
    /// The owned status-bar item; removed from the system status bar on drop.
    item: Retained<NSStatusItem>,
}

// SAFETY: `Retained<NSStatusItem>` is not itself `Send`, but `MacTray` only ever
// dereferences it from a method that has first proved it is on the main thread via
// `MainThreadMarker::new()` (returning `TrayError::NotMainThread` otherwise). The
// handle is moved between threads as an opaque pointer and never messaged off the
// main thread, so transferring ownership is sound. `Tray` requires `Send`.
unsafe impl Send for MacTray {}
// SAFETY: as above — `&MacTray` exposes only main-thread-gated methods; no AppKit
// object is touched without a `MainThreadMarker`, so shared access across threads
// cannot reach AppKit off the main thread. `Tray` requires `Sync`.
unsafe impl Sync for MacTray {}

impl MacTray {
    /// Create a status-bar item in the system status bar.
    ///
    /// Requires a [`MainThreadMarker`] (`NSStatusItem` is main-thread-only), so it
    /// can only be called on the main thread of an AppKit host with a running run
    /// loop; the item appears in the menu bar once that run loop pumps. The button
    /// starts showing the [`TrayState::Disconnected`] label.
    #[must_use]
    pub fn new(mtm: MainThreadMarker) -> Self {
        let status_bar = NSStatusBar::systemStatusBar();
        // A variable-length item sizes itself to its (text) title.
        let item = status_bar.statusItemWithLength(NSVariableStatusItemLength);
        let tray = Self { item };
        // Best-effort initial paint; if the button isn't up yet it's a no-op.
        tray.apply_state(mtm, TrayState::Disconnected);
        tray
    }

    /// The backing menu-bar button, if the item currently has one.
    fn button(&self, mtm: MainThreadMarker) -> Option<Retained<NSStatusBarButton>> {
        self.item.button(mtm)
    }

    /// Write `title` + `tooltip` onto the button. Returns `false` if there is no
    /// button to update.
    fn apply(&self, mtm: MainThreadMarker, title: &str, tooltip: &str) -> bool {
        let Some(button) = self.button(mtm) else {
            return false;
        };
        button.setTitle(&NSString::from_str(title));
        button.setToolTip(Some(&NSString::from_str(tooltip)));
        true
    }

    /// Paint the button for `state` (glyph title + worded tooltip). Returns whether a
    /// button was present to paint.
    fn apply_state(&self, mtm: MainThreadMarker, state: TrayState) -> bool {
        self.apply(mtm, state_label(state), state_tooltip(state))
    }
}

impl Drop for MacTray {
    fn drop(&mut self) {
        // Removing the item is a main-thread AppKit op; only do it on the main thread.
        // If we're not on the main thread the item leaks rather than risk UB — an
        // acceptable, rare cost (a host keeps its tray for the process lifetime).
        if let Some(_mtm) = MainThreadMarker::new() {
            NSStatusBar::systemStatusBar().removeStatusItem(&self.item);
        }
    }
}

/// Map a [`TrayError`] to the erased [`PlatformError`].
fn boxed(e: TrayError) -> PlatformError {
    Box::new(e)
}

impl Tray for MacTray {
    fn set_status(&self, status: &str) -> PlatformResult<()> {
        let mtm = MainThreadMarker::new().ok_or_else(|| boxed(TrayError::NotMainThread))?;
        // The status line is the active-owner indicator: show it as both the button
        // title and its tooltip so it's visible at a glance and on hover.
        if self.apply(mtm, status, status) {
            Ok(())
        } else {
            Err(boxed(TrayError::NoButton))
        }
    }

    fn set_state(&self, state: TrayState) -> PlatformResult<()> {
        let mtm = MainThreadMarker::new().ok_or_else(|| boxed(TrayError::NotMainThread))?;
        if self.apply_state(mtm, state) {
            Ok(())
        } else {
            Err(boxed(TrayError::NoButton))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every state maps to a distinct, non-empty, single-char glyph (so it reads at
    /// menu-bar size) — pure logic, no AppKit, runs headlessly.
    #[test]
    fn state_labels_are_distinct_nonempty_glyphs() {
        let states = [
            TrayState::Owning,
            TrayState::Idle,
            TrayState::Blocked,
            TrayState::Disconnected,
        ];
        let labels: Vec<&str> = states.iter().copied().map(state_label).collect();
        for l in &labels {
            assert!(!l.is_empty(), "label must be non-empty");
            assert_eq!(l.chars().count(), 1, "label should be a single glyph: {l:?}");
        }
        // All four glyphs differ from one another.
        for i in 0..labels.len() {
            for j in (i + 1)..labels.len() {
                assert_ne!(labels[i], labels[j], "labels {i} and {j} collide");
            }
        }
    }

    /// Every state maps to a non-empty, distinct tooltip mentioning "Mouser" — pure
    /// logic, runs headlessly.
    #[test]
    fn state_tooltips_are_distinct_and_branded() {
        let states = [
            TrayState::Owning,
            TrayState::Idle,
            TrayState::Blocked,
            TrayState::Disconnected,
        ];
        let tips: Vec<&str> = states.iter().copied().map(state_tooltip).collect();
        for t in &tips {
            assert!(t.starts_with("Mouser:"), "tooltip should be branded: {t:?}");
        }
        for i in 0..tips.len() {
            for j in (i + 1)..tips.len() {
                assert_ne!(tips[i], tips[j], "tooltips {i} and {j} collide");
            }
        }
    }

    /// `TrayError` renders distinct, human-readable messages — pure, headless.
    #[test]
    fn tray_error_messages_are_distinct() {
        let a = TrayError::NotMainThread.to_string();
        let b = TrayError::NoButton.to_string();
        assert!(!a.is_empty() && !b.is_empty());
        assert_ne!(a, b);
    }

    /// `MacTray` is `Send + Sync` (the `Tray` trait requires it). Compile-time check
    /// of the `unsafe impl`s; no AppKit needed.
    #[test]
    fn mac_tray_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<MacTray>();
    }

    /// Construct a real `MacTray` and drive both trait methods. Needs the main thread
    /// of a process with a running `NSApplication`/run loop and a WindowServer
    /// session, which `cargo test` does not provide (tests run on worker threads), so
    /// it is `#[ignore]`d by default and only meaningful under an AppKit host.
    #[test]
    #[ignore = "needs main thread + running NSApplication/run loop (AppKit host)"]
    fn live_tray_roundtrip() {
        let Some(mtm) = MainThreadMarker::new() else {
            return; // Not on the main thread under the test harness; nothing to do.
        };
        let tray = MacTray::new(mtm);
        tray.set_state(TrayState::Owning).expect("set_state owning");
        tray.set_status("OWNER: laptop").expect("set_status");
    }
}
