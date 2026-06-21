//! Display geometry helpers (read-only) via Core Graphics.
//!
//! The wire protocol addresses motion in a target **display's** logical-pixel
//! space (`display_id`, §7.6 / Appendix A `Monitor`). The spike only needs the
//! main display's bounds to drive the demo square; multi-display enumeration is
//! deferred to the core-trait reconciliation.

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
        [
            (left, top),
            (right, top),
            (right, bottom),
            (left, bottom),
        ]
    }
}

/// Bounds of the main display.
#[must_use]
pub fn main_display_bounds() -> DisplayBounds {
    let main = CGDisplay::main();
    DisplayBounds::from_cg(main.id, main.bounds())
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
}
