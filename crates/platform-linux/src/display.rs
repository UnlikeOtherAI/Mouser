//! Linux display geometry through X11 RandR.
//!
//! This module implements the X11 backend for Mouser's display-local coordinate
//! contract: RandR enumerates active outputs and XQueryPointer returns the real
//! pointer in the root window's global coordinate space. Wayland compositors do
//! not expose an equivalent stable absolute-pointer API through this backend; a
//! future wlr-output-management/data-control path can live beside this one.

use mouser_core::platform::{LocalInputEvent, PlatformError, PlatformResult};
use x11rb::connection::Connection as _;
use x11rb::protocol::randr::{
    Connection as RandrConnectionState, ConnectionExt as RandrConnectionExt,
};
use x11rb::protocol::xproto::{ConnectionExt as XprotoConnectionExt, Window};
use x11rb::rust_connection::RustConnection;

/// Bounds of one active X11 RandR output in root-window pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DisplayBounds {
    /// Zero-based id from the current deterministic RandR output order.
    pub id: u32,
    /// Left edge in root-window pixels.
    pub left: i32,
    /// Top edge in root-window pixels.
    pub top: i32,
    /// Width in pixels.
    pub width: i32,
    /// Height in pixels.
    pub height: i32,
}

impl DisplayBounds {
    #[must_use]
    pub fn local_to_global(self, x: i32, y: i32) -> (i32, i32) {
        let x = x.clamp(0, self.width.saturating_sub(1).max(0));
        let y = y.clamp(0, self.height.saturating_sub(1).max(0));
        (self.left.saturating_add(x), self.top.saturating_add(y))
    }

    #[must_use]
    pub fn contains_global(self, x: i32, y: i32) -> bool {
        x >= self.left
            && x < self.left.saturating_add(self.width)
            && y >= self.top
            && y < self.top.saturating_add(self.height)
    }

    #[must_use]
    pub fn global_to_local(self, x: i32, y: i32) -> (i32, i32) {
        (x.saturating_sub(self.left), y.saturating_sub(self.top))
    }
}

/// Union of all active outputs, used to normalize uinput absolute coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DesktopBounds {
    pub left: i32,
    pub top: i32,
    pub width: i32,
    pub height: i32,
}

impl DesktopBounds {
    #[must_use]
    pub fn from_displays(displays: &[DisplayBounds]) -> Option<Self> {
        let mut iter = displays.iter().copied();
        let first = iter.next()?;
        let mut left = first.left;
        let mut top = first.top;
        let mut right = first.left.saturating_add(first.width);
        let mut bottom = first.top.saturating_add(first.height);
        for display in iter {
            left = left.min(display.left);
            top = top.min(display.top);
            right = right.max(display.left.saturating_add(display.width));
            bottom = bottom.max(display.top.saturating_add(display.height));
        }
        Some(Self {
            left,
            top,
            width: right.saturating_sub(left).max(1),
            height: bottom.saturating_sub(top).max(1),
        })
    }

    #[must_use]
    pub fn scale_x(self, x: i32, abs_max: i32) -> i32 {
        scale_axis(x.saturating_sub(self.left), self.width, abs_max)
    }

    #[must_use]
    pub fn scale_y(self, y: i32, abs_max: i32) -> i32 {
        scale_axis(y.saturating_sub(self.top), self.height, abs_max)
    }
}

fn scale_axis(value: i32, length: i32, abs_max: i32) -> i32 {
    let max_index = length.saturating_sub(1).max(0);
    if max_index == 0 {
        return 0;
    }
    let value = value.clamp(0, max_index);
    ((i64::from(value) * i64::from(abs_max)) / i64::from(max_index)) as i32
}

/// Reusable X11 connection for display and pointer queries.
pub struct X11Display {
    conn: RustConnection,
    root: Window,
}

impl X11Display {
    pub fn connect() -> PlatformResult<Self> {
        let (conn, screen_num) = x11rb::connect(None).map_err(boxed)?;
        let root = root_for(&conn, screen_num)?;
        Ok(Self { conn, root })
    }

    pub fn active_display_bounds(&self) -> PlatformResult<Vec<DisplayBounds>> {
        let resources = self
            .conn
            .randr_get_screen_resources_current(self.root)
            .map_err(boxed)?
            .reply()
            .map_err(boxed)?;
        let mut displays = Vec::with_capacity(resources.outputs.len());
        for output in resources.outputs {
            let info = self
                .conn
                .randr_get_output_info(output, resources.config_timestamp)
                .map_err(boxed)?
                .reply()
                .map_err(boxed)?;
            if info.connection != RandrConnectionState::CONNECTED || info.crtc == 0 {
                continue;
            }
            let crtc = self
                .conn
                .randr_get_crtc_info(info.crtc, resources.config_timestamp)
                .map_err(boxed)?
                .reply()
                .map_err(boxed)?;
            let width = i32::from(crtc.width);
            let height = i32::from(crtc.height);
            if width <= 0 || height <= 0 {
                continue;
            }
            displays.push(DisplayBounds {
                id: output,
                left: i32::from(crtc.x),
                top: i32::from(crtc.y),
                width,
                height,
            });
        }
        displays.sort_by_key(|display| (display.top, display.left, display.id));
        for (id, display) in (0_u32..).zip(displays.iter_mut()) {
            display.id = id;
        }
        Ok(displays)
    }

    pub fn cursor_global_position(&self) -> PlatformResult<(i32, i32)> {
        let pointer = self
            .conn
            .query_pointer(self.root)
            .map_err(boxed)?
            .reply()
            .map_err(boxed)?;
        Ok((i32::from(pointer.root_x), i32::from(pointer.root_y)))
    }
}

fn root_for(conn: &RustConnection, screen_num: usize) -> PlatformResult<Window> {
    conn.setup()
        .roots
        .get(screen_num)
        .map(|screen| screen.root)
        .ok_or_else(|| boxed(InvalidScreen(screen_num)))
}

/// Enumerate active X11 RandR outputs.
pub fn active_display_bounds() -> PlatformResult<Vec<DisplayBounds>> {
    X11Display::connect()?.active_display_bounds()
}

/// Bounds of one active output by its wire `display_id`.
pub fn display_bounds(display_id: u32) -> PlatformResult<DisplayBounds> {
    active_display_bounds()?
        .into_iter()
        .find(|display| display.id == display_id)
        .ok_or_else(|| boxed(UnknownDisplay(display_id)))
}

/// Union of all active output bounds.
pub fn virtual_desktop_bounds() -> PlatformResult<DesktopBounds> {
    let displays = active_display_bounds()?;
    DesktopBounds::from_displays(&displays).ok_or_else(|| boxed(NoActiveDisplays))
}

#[must_use]
pub(crate) fn global_point_to_event(displays: &[DisplayBounds], x: i32, y: i32) -> LocalInputEvent {
    let bounds = displays
        .iter()
        .copied()
        .find(|display| display.contains_global(x, y))
        .or_else(|| displays.first().copied());
    match bounds {
        Some(bounds) => {
            let (x, y) = bounds.global_to_local(x, y);
            LocalInputEvent::CursorMoved {
                display_id: bounds.id,
                x,
                y,
            }
        }
        None => LocalInputEvent::CursorMoved {
            display_id: 0,
            x,
            y,
        },
    }
}

fn boxed<E: std::error::Error + Send + Sync + 'static>(e: E) -> PlatformError {
    Box::new(e)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnknownDisplay(pub u32);

impl std::fmt::Display for UnknownDisplay {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "no active Linux display with id {}", self.0)
    }
}

impl std::error::Error for UnknownDisplay {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NoActiveDisplays;

impl std::fmt::Display for NoActiveDisplays {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "X11 RandR reported no active outputs")
    }
}

impl std::error::Error for NoActiveDisplays {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InvalidScreen(pub usize);

impl std::fmt::Display for InvalidScreen {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "X11 connection returned invalid screen index {}", self.0)
    }
}

impl std::error::Error for InvalidScreen {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_global_mapping_clamps_to_output() {
        let bounds = DisplayBounds {
            id: 9,
            left: 1920,
            top: 100,
            width: 1280,
            height: 720,
        };
        assert_eq!(bounds.local_to_global(0, 0), (1920, 100));
        assert_eq!(bounds.local_to_global(50, 20), (1970, 120));
        assert_eq!(bounds.local_to_global(5000, 5000), (3199, 819));
        assert_eq!(bounds.local_to_global(-5, -5), (1920, 100));
    }

    #[test]
    fn containing_output_maps_to_local_coords() {
        let displays = [
            DisplayBounds {
                id: 1,
                left: 0,
                top: 0,
                width: 1920,
                height: 1080,
            },
            DisplayBounds {
                id: 7,
                left: 1920,
                top: 0,
                width: 1280,
                height: 1024,
            },
        ];
        assert_eq!(
            global_point_to_event(&displays, 2020, 50),
            LocalInputEvent::CursorMoved {
                display_id: 7,
                x: 100,
                y: 50
            }
        );
    }

    #[test]
    fn desktop_union_scales_to_absolute_range() {
        let displays = [
            DisplayBounds {
                id: 1,
                left: 0,
                top: 0,
                width: 1920,
                height: 1080,
            },
            DisplayBounds {
                id: 2,
                left: 1920,
                top: 0,
                width: 1280,
                height: 1024,
            },
        ];
        let desktop = DesktopBounds::from_displays(&displays);
        assert_eq!(desktop.map(|bounds| bounds.scale_x(0, 100)), Some(0));
        assert_eq!(desktop.map(|bounds| bounds.scale_x(3199, 100)), Some(100));
        assert_eq!(desktop.map(|bounds| bounds.scale_y(1079, 100)), Some(100));
    }
}
