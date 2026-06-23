//! Non-Windows stub of the Windows clipboard adapter.
//!
//! The real [`crate::clipboard`] backend is `#[cfg(target_os = "windows")]` (it
//! calls the Win32 clipboard API). This stub gives the rest of the workspace a
//! `WinClipboard` type that implements [`mouser_core::platform::Clipboard`] on
//! macOS / Linux hosts too — mirroring `platform-linux`'s `LinuxClipboard` stub —
//! so cross-platform code and tests can name and exercise the type everywhere.
//! Every operation returns a typed [`Unsupported`] error.

#![cfg(not(target_os = "windows"))]

use mouser_core::platform::{ClipFormat, Clipboard, PlatformResult};

/// Non-Windows stub of the Win32 clipboard adapter (no clipboard backend off
/// Windows). Present so dependent code type-checks on every host.
#[derive(Debug, Default, Clone, Copy)]
pub struct WinClipboard;

impl WinClipboard {
    /// A stub clipboard adapter (no Win32 backend off Windows).
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    /// Always `0` off-target: there is no Windows clipboard sequence number.
    #[must_use]
    pub fn change_count(&self) -> i64 {
        0
    }
}

impl Clipboard for WinClipboard {
    fn change_token(&self) -> PlatformResult<u64> {
        Ok(0)
    }

    fn read(&self, _format: ClipFormat) -> PlatformResult<Option<Vec<u8>>> {
        Err(Box::new(Unsupported))
    }

    fn write(&self, _format: ClipFormat, _data: &[u8]) -> PlatformResult<()> {
        Err(Box::new(Unsupported))
    }
}

/// The Windows clipboard backend was called on a non-Windows host (stub build).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Unsupported;

impl std::fmt::Display for Unsupported {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "the Windows clipboard backend is only available on Windows"
        )
    }
}

impl std::error::Error for Unsupported {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stub_reports_unsupported() {
        let cb = WinClipboard::new();
        let err = cb.read(ClipFormat::Html).expect_err("stub must error");
        assert!(err.downcast_ref::<Unsupported>().is_some());
        let werr = cb
            .write(ClipFormat::Html, b"<b>x</b>")
            .expect_err("stub write must error");
        assert!(werr.downcast_ref::<Unsupported>().is_some());
        assert_eq!(cb.change_count(), 0);
    }
}
