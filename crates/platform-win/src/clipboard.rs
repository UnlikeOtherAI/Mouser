//! Windows clipboard adapter — Win32-backed [`mouser_core::platform::Clipboard`].
//!
//! [`WinClipboard`] moves raw bytes to/from the system clipboard for a given
//! [`ClipFormat`] using the classic Win32 clipboard API
//! (`OpenClipboard`/`GetClipboardData`/`SetClipboardData`) over `HGLOBAL` memory.
//! It does **no** canonicalization, hashing, dedup, or loop-prevention — per spec
//! §7.7 that lives in the engine; this is a thin byte mover for one format.
//!
//! ## Format mapping (spec §7.7 / Appendix C)
//! | `ClipFormat` | Win32 clipboard format                                  |
//! |--------------|---------------------------------------------------------|
//! | `Utf8Text`   | `CF_UNICODETEXT` (UTF-16LE on the clipboard; we transcode to/from UTF-8) |
//! | `Html`       | registered `"HTML Format"` (CF_HTML: the wire's raw fragment wrapped in the required description header; see [`crate::cfhtml`]) |
//! | `Rtf`        | registered `"Rich Text Format"` (bytes as-is)         |
//! | `Png`        | registered `"PNG"` (raw PNG byte stream)              |
//! | `UriList`    | `CF_HDROP` ↔ a `text/uri-list` of `file://` URIs       |
//!
//! `Utf8Text` is the one format the clipboard stores in a different encoding than
//! the wire: Windows uses UTF-16LE (`CF_UNICODETEXT`) with CRLF line endings, so
//! we transcode both ways while keeping the engine-facing form UTF-8/LF.
//! `Html` is wrapped into / unwrapped from the CF_HTML description-header format
//! ([`crate::cfhtml`]) so a round-trip with the raw-fragment mac/linux clipboards
//! interoperates. `Rtf`/`Png` are opaque bytes carried verbatim. `UriList` bridges the wire
//! `text/uri-list` (LF-separated `file://` URIs, spec §7.7) and the native
//! `CF_HDROP` file-drop list; see [`hdrop`].
//!
//! ## Change detection
//! [`WinClipboard::change_count`] returns `GetClipboardSequenceNumber`, which the OS
//! bumps on every clipboard change. A poller compares it to the last seen value to
//! learn the clipboard changed (spec §7.7); the value alone says nothing about what.
//! `0` is returned when the process lacks `WINSTA_ACCESSCLIPBOARD` — treat a *stuck*
//! `0` as "cannot observe" rather than "never changed".
//!
//! ## `unsafe`
//! All Win32 calls are `unsafe`; each carries a `// SAFETY:` note. The RAII
//! [`ClipboardSession`] guarantees `CloseClipboard` runs even on early return, and
//! the `HGLOBAL` lock/unlock is balanced within a single function so no pointer
//! escapes its `GlobalLock`/`GlobalUnlock` pair.

#![cfg(target_os = "windows")]

use mouser_core::platform::{ClipFormat, Clipboard, PlatformError, PlatformResult};
use windows::core::PCWSTR;
use windows::Win32::Foundation::{GlobalFree, HANDLE, HGLOBAL};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, GetClipboardData, GetClipboardSequenceNumber,
    IsClipboardFormatAvailable, OpenClipboard, RegisterClipboardFormatW, SetClipboardData,
};
use windows::Win32::System::Memory::{
    GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock, GMEM_MOVEABLE, GMEM_ZEROINIT,
};
use windows::Win32::System::Ole::{CF_HDROP, CF_UNICODETEXT};

use crate::clipboard_text::{
    cf_unicodetext_bytes_to_engine_utf8, engine_utf8_to_cf_unicodetext_bytes,
};

mod hdrop;

/// Win32-backed [`Clipboard`] over the system clipboard.
///
/// Zero-sized: the clipboard is a process-global OS resource, so there is no
/// per-instance state.
#[derive(Debug, Default, Clone, Copy)]
pub struct WinClipboard;

impl WinClipboard {
    /// A clipboard adapter bound to the system clipboard.
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    /// `GetClipboardSequenceNumber` — the OS clipboard change counter.
    ///
    /// Increments on every clipboard mutation. A poller treats any increase as
    /// "the clipboard changed" (spec §7.7). Returns `0` if the station denies
    /// clipboard access; callers should treat a persistently-zero value as
    /// "unobservable", not "unchanged".
    #[must_use]
    pub fn change_count(&self) -> i64 {
        // SAFETY: no arguments, no out-params; returns the current sequence number
        // (or 0 without `WINSTA_ACCESSCLIPBOARD`). Always safe to call.
        let n = unsafe { GetClipboardSequenceNumber() };
        i64::from(n)
    }
}

impl Clipboard for WinClipboard {
    fn change_token(&self) -> PlatformResult<u64> {
        match u64::try_from(self.change_count()) {
            Ok(token) => Ok(token),
            Err(_) => Ok(0),
        }
    }

    fn read(&self, format: ClipFormat) -> PlatformResult<Option<Vec<u8>>> {
        let fmt = clipboard_format(format)?;
        let _session = ClipboardSession::open()?;

        // Absent format → `None` (not an error): the clipboard simply has no such rep.
        // SAFETY: format is a valid registered/standard id; returns Ok only when present.
        if unsafe { IsClipboardFormatAvailable(fmt) }.is_err() {
            return Ok(None);
        }

        // SAFETY: the clipboard is open (held by `_session`); `GetClipboardData`
        // returns a NON-owned handle valid until `CloseClipboard`, so we must copy
        // its bytes out before the session drops. We never free this handle.
        let handle = match unsafe { GetClipboardData(fmt) } {
            Ok(h) if !h.is_invalid() => h,
            _ => return Ok(None),
        };

        let raw = read_hglobal_bytes(handle)?;
        let decoded = match format {
            ClipFormat::Utf8Text => cf_unicodetext_bytes_to_engine_utf8(&raw),
            ClipFormat::UriList => hdrop::hdrop_bytes_to_uri_list(handle)?,
            // CF_HTML carries a description header around the fragment; strip it
            // so the wire/mac/linux raw-fragment representation is returned.
            ClipFormat::Html => crate::cfhtml::decode(&raw),
            _ => raw,
        };
        Ok(Some(decoded))
    }

    fn write(&self, format: ClipFormat, data: &[u8]) -> PlatformResult<()> {
        let fmt = clipboard_format(format)?;

        // Build the exact `HGLOBAL` payload Windows expects for this format *before*
        // taking ownership of the clipboard, so a conversion error can't leave it empty.
        let payload: Vec<u8> = match format {
            ClipFormat::Utf8Text => {
                let s = std::str::from_utf8(data).map_err(|e| -> PlatformError { Box::new(e) })?;
                engine_utf8_to_cf_unicodetext_bytes(s)
            }
            ClipFormat::UriList => hdrop::uri_list_to_hdrop_bytes(data)?,
            // Windows CF_HTML requires the description header (Version/StartHTML/
            // …/EndFragment offsets) wrapping the raw fragment we carry on the wire.
            ClipFormat::Html => crate::cfhtml::encode(data),
            _ => data.to_vec(),
        };

        let _session = ClipboardSession::open()?;
        let hglobal = alloc_global(&payload)?;
        // SAFETY: clipboard is open; `EmptyClipboard` takes ownership for this process,
        // a precondition for `SetClipboardData`.
        if let Err(e) = unsafe { EmptyClipboard() } {
            // SAFETY: `hglobal` is our still-owned, unlocked allocation.
            let _ = unsafe { GlobalFree(Some(hglobal)) };
            return Err(boxed(e));
        }

        match set_clipboard_data_immediate(fmt, hglobal) {
            Ok(()) => Ok(()),
            Err(e) => {
                // Ownership did not transfer; reclaim the block so it doesn't leak.
                // SAFETY: `hglobal` is our still-owned, unlocked allocation.
                let _ = unsafe { GlobalFree(Some(hglobal)) };
                Err(boxed(e))
            }
        }
    }
}

/// Box a `windows::core::Error` as a [`PlatformError`].
fn boxed(e: windows::core::Error) -> PlatformError {
    Box::new(e)
}

/// Resolve a [`ClipFormat`] to its numeric Win32 clipboard format id.
///
/// Standard ids (`CF_UNICODETEXT`, `CF_HDROP`) are constants; `Html`/`Rtf`/`Png`
/// are *registered* formats whose ids are assigned by the OS at first use and stable
/// for the session. A `0` from `RegisterClipboardFormatW` means registration failed.
/// The clipboard APIs all take the format as a bare `u32`.
fn clipboard_format(format: ClipFormat) -> PlatformResult<u32> {
    let id = match format {
        ClipFormat::Utf8Text => u32::from(CF_UNICODETEXT.0),
        ClipFormat::UriList => u32::from(CF_HDROP.0),
        ClipFormat::Html => register("HTML Format")?,
        ClipFormat::Rtf => register("Rich Text Format")?,
        ClipFormat::Png => register("PNG")?,
    };
    Ok(id)
}

/// Register (or look up) a named clipboard format, returning its id.
fn register(name: &str) -> PlatformResult<u32> {
    let wide: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
    // SAFETY: `wide` is a NUL-terminated UTF-16 buffer that outlives the call.
    let id = unsafe { RegisterClipboardFormatW(PCWSTR(wide.as_ptr())) };
    if id == 0 {
        return Err(Box::new(ClipboardError::RegisterFailed(name.to_owned())));
    }
    Ok(id)
}

/// RAII guard around `OpenClipboard`/`CloseClipboard`.
///
/// Opening with a `None` owner is intentional here because this adapter only uses
/// immediate rendering: every `SetClipboardData` call passes an allocated
/// `HGLOBAL`, never delayed-render `None`. A future delayed-render path must use a
/// real owner window. The guard's `Drop` runs `CloseClipboard` even on an early `?`
/// return, so the clipboard is never left open (which would lock every other app
/// out of it).
struct ClipboardSession;

impl ClipboardSession {
    /// Open the clipboard for this task.
    fn open() -> PlatformResult<Self> {
        // SAFETY: passing `None` is valid for immediate-render clipboard writes. On
        // failure (another process holds it) we return an error and do not close.
        unsafe { OpenClipboard(None) }.map_err(boxed)?;
        Ok(Self)
    }
}

impl Drop for ClipboardSession {
    fn drop(&mut self) {
        // SAFETY: balanced with the successful `OpenClipboard` in `open`; ignore the
        // result since `Drop` cannot propagate and there is no recovery.
        let _ = unsafe { CloseClipboard() };
    }
}

/// Copy the bytes of a clipboard `HGLOBAL` handle into an owned `Vec`.
///
/// The handle is locked to obtain a pointer + size, the bytes are copied, then the
/// handle is unlocked. The handle itself is owned by the clipboard, not us.
fn read_hglobal_bytes(handle: HANDLE) -> PlatformResult<Vec<u8>> {
    let hglobal = HGLOBAL(handle.0);
    // SAFETY: `hglobal` came from `GetClipboardData` and is valid while the clipboard
    // is open; `GlobalSize` reads its allocation size.
    let size = unsafe { GlobalSize(hglobal) };
    if size == 0 {
        return Ok(Vec::new());
    }
    // SAFETY: locks the block, returning a readable pointer to `size` bytes (or null
    // on failure). We read exactly `size` bytes and unlock before returning.
    let ptr = unsafe { GlobalLock(hglobal) } as *const u8;
    if ptr.is_null() {
        return Err(Box::new(ClipboardError::LockFailed));
    }
    // SAFETY: `ptr` is valid for `size` bytes per the successful lock above; the
    // source (clipboard memory) and destination (fresh Vec) do not overlap.
    let bytes = unsafe { std::slice::from_raw_parts(ptr, size) }.to_vec();
    // SAFETY: balances the `GlobalLock` above on the same handle.
    let _ = unsafe { GlobalUnlock(hglobal) };
    Ok(bytes)
}

/// Allocate a moveable `HGLOBAL` and copy `bytes` into it (for `SetClipboardData`).
///
/// `SetClipboardData` requires `GMEM_MOVEABLE` memory and, on success, takes
/// ownership of it. The block is unlocked before returning so the clipboard can
/// relocate it.
///
/// An **empty** payload allocates a zero-length block (no lock/copy): `GlobalLock`
/// of a zero-length object returns null, and forcing a 1-byte allocation (the
/// prior `len.max(1)`) published a stale, uninitialized byte that reads observed
/// as `GlobalSize == 1` — an empty write would not round-trip to empty. A
/// zero-length handle is valid for `SetClipboardData`, and `GlobalSize` reports
/// `0`, so an empty write correctly reads back as empty. `GMEM_ZEROINIT` zeroes
/// any allocator slack so no stale bytes can ever leak into the clipboard.
fn alloc_global(bytes: &[u8]) -> PlatformResult<HGLOBAL> {
    // SAFETY: zero-initialized moveable allocation of exactly `bytes.len()` bytes;
    // returns a valid handle (possibly zero-length) or an error.
    let hglobal =
        unsafe { GlobalAlloc(GMEM_MOVEABLE | GMEM_ZEROINIT, bytes.len()) }.map_err(boxed)?;
    if bytes.is_empty() {
        // Nothing to copy; a zero-length block can't be locked and needs no fill.
        return Ok(hglobal);
    }
    // SAFETY: just-allocated handle; lock to get a writable pointer to >= len bytes.
    let ptr = unsafe { GlobalLock(hglobal) } as *mut u8;
    if ptr.is_null() {
        // SAFETY: reclaim the block we just allocated and still own (lock failed).
        let _ = unsafe { GlobalFree(Some(hglobal)) };
        return Err(Box::new(ClipboardError::LockFailed));
    }
    // SAFETY: `ptr` is valid for `bytes.len()` writes (alloc was `>= len`); regions
    // do not overlap (fresh allocation vs caller's slice).
    unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr, bytes.len()) };
    // SAFETY: balances the lock; the handle remains valid and owned by us until
    // `SetClipboardData` succeeds.
    let _ = unsafe { GlobalUnlock(hglobal) };
    Ok(hglobal)
}

/// Immediate-render `SetClipboardData`.
///
/// This helper intentionally takes a concrete `HGLOBAL` and always passes
/// `Some(handle)` so the `OpenClipboard(None)` session above cannot accidentally be
/// reused for delayed rendering. Delayed rendering requires a real owner window.
fn set_clipboard_data_immediate(format: u32, hglobal: HGLOBAL) -> Result<(), windows::core::Error> {
    // SAFETY: clipboard is open and emptied; on success Windows takes ownership of
    // `hglobal` (we must NOT free it). `HGLOBAL` -> `HANDLE` is a pointer newtype cast.
    unsafe { SetClipboardData(format, Some(HANDLE(hglobal.0))) }.map(|_| ())
}

/// Errors raised by the Windows clipboard adapter that aren't a raw `windows::Error`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClipboardError {
    /// `RegisterClipboardFormatW` returned 0 for the named format.
    RegisterFailed(String),
    /// `GlobalLock` returned null (the handle could not be locked).
    LockFailed,
    /// A `text/uri-list` payload was not valid UTF-8.
    InvalidUriList,
}

impl std::fmt::Display for ClipboardError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::RegisterFailed(n) => write!(f, "RegisterClipboardFormatW failed for {n:?}"),
            Self::LockFailed => write!(f, "GlobalLock returned null"),
            Self::InvalidUriList => write!(f, "text/uri-list payload was not valid UTF-8"),
        }
    }
}

impl std::error::Error for ClipboardError {}
