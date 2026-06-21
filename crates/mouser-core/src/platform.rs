//! Platform trait contracts (architecture §2, §4.6).
//!
//! The per-OS adapters (`platform-mac`, `platform-win`, `platform-linux`) implement
//! these traits; `mouser-core` and `mouser-engine` drive them. **This module is
//! definitions and documentation only** — no platform code, no I/O, no state. Keeping
//! the contracts here lets the core be tested with fakes and lets the engine stay
//! adapter-agnostic.
//!
//! Coordinate convention (spec §7.6, Appendix A): cursor positions are **integer
//! logical pixels** in a specific display's space (`display_id`), origin top-left,
//! y-down. Scaling to native device pixels is the adapter's responsibility.

/// A platform-specific error from an adapter operation.
///
/// Adapters report failures (missing permission, secure desktop, unsupported
/// compositor, transient OS error) as a boxed error; the engine maps recoverable
/// cases to `CapabilityState`/`FocusKind::InputBlocked` per spec §7.4. The trait stays
/// object-safe by erasing the concrete error type.
pub type PlatformError = Box<dyn std::error::Error + Send + Sync + 'static>;

/// Result alias for adapter operations.
pub type PlatformResult<T> = Result<T, PlatformError>;

/// A locally-observed input event surfaced by [`InputCapture`].
///
/// These are *raw local* events (this machine's own hardware). The engine decides
/// whether they cause a local reclaim, an edge crossing, or are forwarded as the
/// owner to a remote peer (spec §7.4–§7.6). Keycodes are USB HID Usage IDs
/// (Usage Page 0x07, Appendix B); buttons follow §7.5 (0=left,1=right,2=middle,
/// 3=back,4=forward).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LocalInputEvent {
    /// Absolute cursor position in logical pixels on a display.
    CursorMoved {
        /// Target display (Appendix A `display_id`).
        display_id: u32,
        /// X in logical pixels, display origin top-left.
        x: i32,
        /// Y in logical pixels, display origin top-left, y-down.
        y: i32,
    },
    /// A pointer button transition.
    Button {
        /// Button index per §7.5.
        button: u8,
        /// `true` = pressed, `false` = released.
        down: bool,
    },
    /// A key transition by HID usage (Usage Page 0x07).
    Key {
        /// USB HID Usage ID (Appendix B).
        usage: u16,
        /// `true` = pressed, `false` = released.
        down: bool,
        /// Active modifier bitmask (Appendix B).
        mods: u16,
    },
    /// A scroll/wheel event.
    Scroll {
        /// Horizontal delta.
        dx: i32,
        /// Vertical delta.
        dy: i32,
    },
}

/// Injects synthetic input into the local OS (the *target* side of a handoff).
///
/// All methods are called only after the engine has authorized the source peer
/// (`negotiated_capability ∧ local_permission`, spec §9) and confirmed current
/// ownership (`owner_epoch`, §7.4). Adapters must not enforce policy themselves; the
/// core gates every call.
pub trait InputInjection: Send + Sync {
    /// Move the cursor to an **absolute logical-pixel** position on `display_id`
    /// (spec §7.6 tag 0x01). Receiver clamps out-of-range coordinates to the display.
    fn move_cursor(&self, display_id: u32, x: i32, y: i32) -> PlatformResult<()>;

    /// Apply a relative cursor delta in logical pixels (spec §7.6 tag 0x02), used
    /// only when the foreground app has grabbed pointer-lock.
    fn move_cursor_relative(&self, dx: i32, dy: i32) -> PlatformResult<()>;

    /// Press or release a pointer button (`down = true` presses). Button index §7.5.
    fn button(&self, button: u8, down: bool) -> PlatformResult<()>;

    /// Press or release a key identified by its USB HID usage (Usage Page 0x07),
    /// with the active modifier bitmask `mods` (Appendix B).
    fn key(&self, usage: u16, down: bool, mods: u16) -> PlatformResult<()>;

    /// Scroll by `(dx, dy)` in the given [`crate::platform::ScrollUnit`] (spec §7.5).
    fn scroll(&self, dx: i32, dy: i32, unit: ScrollUnit) -> PlatformResult<()>;
}

/// Scroll delta unit (mirrors the wire `ScrollUnit`, spec §7.5 / Appendix C).
///
/// Re-stated here so the platform contract does not force adapters to depend on the
/// wire crate. The engine translates between this and the wire enum.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScrollUnit {
    /// `dx/dy` in 1/120-of-a-wheel-notch units (legacy wheel detents).
    Detent120,
    /// High-resolution / trackpad logical pixels.
    LogicalPixel,
}

/// What the adapter should do with a captured local event after the sink has
/// seen it (audit H3).
///
/// In the active-device model a machine that currently owns a *remote* peer must
/// **swallow its own local input** (so the cursor/keys don't also drive the
/// local desktop) while still forwarding the event over the wire. The sink
/// returns this decision per event so the engine — not the adapter — owns the
/// suppress-vs-passthrough policy (spec §7.4, §9).
///
/// Real suppression is a platform capability, not a guarantee: on macOS it
/// requires an active **default** `CGEventTap` (not listen-only) backed by the
/// Accessibility grant; without that the adapter can only observe. When an
/// adapter cannot honor [`CaptureDecision::Suppress`] it must pass the event
/// through and report the reduced capability (e.g. `CapState::PermissionMissing`)
/// rather than silently pretend it suppressed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CaptureDecision {
    /// Let the event continue to the local OS as normal.
    PassThrough,
    /// Swallow the event locally (do not deliver it to the local desktop). Used
    /// while this machine owns a remote peer and is forwarding input.
    Suppress,
}

/// A sink for locally-observed input events delivered by [`InputCapture`].
///
/// The callback runs on the adapter's capture thread and must be cheap and
/// non-blocking; it hands events to the engine, which makes ownership/forwarding
/// decisions off the hot path and replies with a [`CaptureDecision`] telling the
/// adapter whether to swallow the event locally (audit H3).
pub trait InputSink: Send + Sync {
    /// Receive one locally-observed input event and decide its local fate.
    fn on_event(&self, event: LocalInputEvent) -> CaptureDecision;
}

/// Captures local input so the engine can detect edge crossings, local reclaim, and
/// the panic hotkey, and forward events while this machine is the owner.
///
/// For each event the adapter calls [`InputSink::on_event`] and honors the
/// returned [`CaptureDecision`]. Whether [`CaptureDecision::Suppress`] is
/// actually enforceable is platform- and permission-dependent; adapters expose
/// that via [`InputCapture::can_suppress`] so the engine can downgrade behavior
/// instead of assuming local input is gone (audit H3).
pub trait InputCapture: Send + Sync {
    /// Begin capturing local input, delivering events to `sink`. Idempotent: calling
    /// `start` while already capturing is a no-op.
    fn start(&self, sink: std::sync::Arc<dyn InputSink>) -> PlatformResult<()>;

    /// Stop capturing local input. Idempotent.
    fn stop(&self) -> PlatformResult<()>;

    /// Whether this capture backend can actually swallow local input
    /// ([`CaptureDecision::Suppress`]) in its current state. `false` means
    /// suppression requests are observed but pass through (e.g. listen-only tap,
    /// or missing Accessibility on macOS); the engine should surface the reduced
    /// capability rather than rely on suppression.
    fn can_suppress(&self) -> bool;
}

/// Read and write the system clipboard (spec §7.7).
///
/// Payload transport, hashing, dedup, and loop-prevention live in the engine; the
/// adapter only moves bytes to/from the OS clipboard for a given format.
pub trait Clipboard: Send + Sync {
    /// Read the current clipboard contents for `format`, or `None` if absent.
    fn read(&self, format: ClipFormat) -> PlatformResult<Option<Vec<u8>>>;

    /// Replace the clipboard contents for `format` with `data`.
    fn write(&self, format: ClipFormat, data: &[u8]) -> PlatformResult<()>;
}

/// Clipboard payload format (mirrors the wire `ClipFormat`, spec §7.7 / Appendix C).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClipFormat {
    /// UTF-8 text (CRLF normalized to LF on the wire).
    Utf8Text,
    /// HTML fragment, bytes as-is.
    Html,
    /// PNG image, raw byte stream (rides the bulk connection).
    Png,
    /// `text/uri-list`, LF-separated.
    UriList,
    /// Rich Text Format, bytes as-is.
    Rtf,
}

/// The OS tray / menu-bar surface owned by the engine (architecture §2, §4).
///
/// The Tauri UI is a separate process; the engine still owns a minimal tray for
/// status and the active-owner indicator (spec §9). Methods are intentionally small.
pub trait Tray: Send + Sync {
    /// Set the tray status line / tooltip (e.g. the active-owner indicator).
    fn set_status(&self, status: &str) -> PlatformResult<()>;

    /// Set the tray icon to reflect the given [`TrayState`].
    fn set_state(&self, state: TrayState) -> PlatformResult<()>;
}

/// Visible state shown by the [`Tray`] (architecture §4, spec §9 owner indicator).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrayState {
    /// This machine currently owns input.
    Owning,
    /// Connected to the cluster but not the input owner.
    Idle,
    /// Input is blocked (secure desktop, lock screen, missing permission).
    Blocked,
    /// Not connected to any peer.
    Disconnected,
}
