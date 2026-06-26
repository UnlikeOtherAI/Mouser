//! macOS [`InputInjection`] adapter wrapper.
//!
//! The low-level Core Graphics calls live in [`crate::inject`]. This module keeps
//! Mouser's platform trait implementation small: display-local cursor translation,
//! scroll unit conversion, and Cmd/Ctrl preference handling.

use mouser_core::platform::{InputInjection, PlatformError, PlatformResult, ScrollUnit};

use crate::display_info::{display_bounds, main_display_bounds};
use crate::inject;

/// macOS input injector. Every injection call posts a fresh `CGEvent`.
///
/// `cmd_ctrl_swap` is the cluster input preference (Appendix A `input_prefs`);
/// when set, a remote machine's Ctrl is delivered as Cmd and vice-versa.
#[derive(Debug, Clone)]
pub struct MacInjector {
    cmd_ctrl_swap: bool,
}

impl Default for MacInjector {
    fn default() -> Self {
        Self::new()
    }
}

impl MacInjector {
    /// Injector with the default (no) Cmd/Ctrl swap.
    #[must_use]
    pub fn new() -> Self {
        Self {
            cmd_ctrl_swap: false,
        }
    }

    /// Injector with the Cmd/Ctrl swap preference set.
    #[must_use]
    pub fn with_cmd_ctrl_swap(cmd_ctrl_swap: bool) -> Self {
        Self { cmd_ctrl_swap }
    }
}

fn boxed(e: inject::InjectError) -> PlatformError {
    Box::new(e)
}

impl InputInjection for MacInjector {
    fn move_cursor(&self, display_id: u32, x: i32, y: i32) -> PlatformResult<()> {
        // The source addresses the target's primary display as id 0 because it cannot
        // know the target's real CG display ids. A CG id can also go stale after a
        // display reconfigure, so fall back to the main display instead of tripping
        // injection failure and ownership recovery.
        let bounds = display_bounds(display_id).unwrap_or_else(main_display_bounds);
        let (gx, gy) = bounds.local_to_global(x, y);
        inject::move_cursor(gx, gy).map_err(boxed)
    }

    fn move_cursor_relative(&self, dx: i32, dy: i32) -> PlatformResult<()> {
        inject::move_cursor_rel(dx, dy).map_err(boxed)
    }

    fn button(&self, button: u8, down: bool) -> PlatformResult<()> {
        inject::button(button, down).map_err(boxed)
    }

    fn key(&self, usage: u16, down: bool, mods: u16) -> PlatformResult<()> {
        inject::key_press(usage, down, mods, self.cmd_ctrl_swap).map_err(boxed)
    }

    fn scroll(&self, dx: i32, dy: i32, unit: ScrollUnit) -> PlatformResult<()> {
        // `Detent120` is line/notch-based; `LogicalPixel` is pixel-precise.
        let pixel = matches!(unit, ScrollUnit::LogicalPixel);
        let (dx, dy) = match unit {
            ScrollUnit::Detent120 => (dx / 120, dy / 120),
            ScrollUnit::LogicalPixel => (dx, dy),
        };
        inject::scroll(dx, dy, pixel).map_err(boxed)
    }

    fn set_cursor_visible(&self, visible: bool) -> PlatformResult<()> {
        if visible {
            inject::set_cursor_visible(true).map_err(boxed)?;
        }
        Ok(())
    }
}
