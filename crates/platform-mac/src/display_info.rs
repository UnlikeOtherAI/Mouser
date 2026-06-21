//! Display geometry helpers (read-only) via Core Graphics.
//!
//! The wire protocol addresses motion in a target **display's** logical-pixel
//! space (`display_id`, §7.6 / Appendix A `Monitor`); the CG direct display id is
//! that `display_id`. [`display_bounds`] enumerates **all** active displays
//! (`CGGetActiveDisplayList`, via `CGDisplay::active_displays`) so the injector
//! can translate display-local coordinates to the global CG space on a
//! multi-monitor setup (audit M1), not just the main display.

use core_graphics::display::CGDisplay;
use core_graphics::geometry::CGRect;

/// Rectangle of a display in global, top-left-origin point coordinates.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DisplayBounds {
    /// CG direct display id.
    pub id: u32,
    /// Left edge (global points).
    pub x: f64,
    /// Top edge (global points).
    pub y: f64,
    /// Width in points.
    pub w: f64,
    /// Height in points.
    pub h: f64,
}

impl DisplayBounds {
    fn from_cg(id: u32, rect: CGRect) -> Self {
        Self {
            id,
            x: rect.origin.x,
            y: rect.origin.y,
            w: rect.size.width,
            h: rect.size.height,
        }
    }

    /// Four inset corners (top-left, top-right, bottom-right, bottom-left),
    /// `inset` points in from each edge — a visible square for the demo.
    #[must_use]
    pub fn inset_corners(&self, inset: f64) -> [(f64, f64); 4] {
        let left = self.x + inset;
        let right = self.x + self.w - inset;
        let top = self.y + inset;
        let bottom = self.y + self.h - inset;
        [(left, top), (right, top), (right, bottom), (left, bottom)]
    }

    /// Map a **display-local** logical-pixel `(x, y)` (origin top-left, y-down)
    /// to a **global** CG point, clamping to this display's bounds.
    ///
    /// Out-of-range coordinates are clamped to the last in-bounds pixel
    /// (`w-1`/`h-1`), matching the wire contract "receiver clamps" (§7.6).
    #[must_use]
    pub fn local_to_global(&self, x: i32, y: i32) -> (f64, f64) {
        let lx = f64::from(x).clamp(0.0, (self.w - 1.0).max(0.0));
        let ly = f64::from(y).clamp(0.0, (self.h - 1.0).max(0.0));
        (self.x + lx, self.y + ly)
    }
}

/// Bounds of the main display.
#[must_use]
pub fn main_display_bounds() -> DisplayBounds {
    let main = CGDisplay::main();
    DisplayBounds::from_cg(main.id, main.bounds())
}

/// Bounds of **every** active display (`CGGetActiveDisplayList`), in global
/// top-left-origin point coordinates.
///
/// Returns an empty vec if CG reports no active displays (e.g. headless).
#[must_use]
pub fn active_display_bounds() -> Vec<DisplayBounds> {
    let ids = CGDisplay::active_displays().unwrap_or_default();
    ids.into_iter()
        .map(|id| {
            let d = CGDisplay::new(id);
            DisplayBounds::from_cg(id, d.bounds())
        })
        .collect()
}

/// Bounds of the display whose CG id equals `display_id`, or `None` if no active
/// display matches (audit M1: full enumeration, not just the main display).
#[must_use]
pub fn display_bounds(display_id: u32) -> Option<DisplayBounds> {
    active_display_bounds()
        .into_iter()
        .find(|b| b.id == display_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn corners_form_a_square_inside_bounds() {
        let b = DisplayBounds {
            id: 1,
            x: 0.0,
            y: 0.0,
            w: 1000.0,
            h: 800.0,
        };
        let c = b.inset_corners(50.0);
        assert_eq!(c[0], (50.0, 50.0));
        assert_eq!(c[1], (950.0, 50.0));
        assert_eq!(c[2], (950.0, 750.0));
        assert_eq!(c[3], (50.0, 750.0));
    }

    #[test]
    fn local_to_global_offsets_by_display_origin() {
        // A secondary display to the right of the main one.
        let b = DisplayBounds {
            id: 7,
            x: 1920.0,
            y: 0.0,
            w: 1280.0,
            h: 1024.0,
        };
        // Display-local (0,0) -> the display's global origin.
        assert_eq!(b.local_to_global(0, 0), (1920.0, 0.0));
        // A point inside.
        assert_eq!(b.local_to_global(100, 50), (2020.0, 50.0));
        // Out-of-range clamps to w-1 / h-1 (then offset).
        assert_eq!(b.local_to_global(5000, 5000), (1920.0 + 1279.0, 1023.0));
        // Negative clamps to the origin.
        assert_eq!(b.local_to_global(-10, -10), (1920.0, 0.0));
    }
}
