//! `win_injector_display_demo` — exercises the `WinInjector` trait path that Mouser
//! uses as a target.
//!
//! This is the manual proof for display id routing: `display_id = 0` must resolve to
//! the Windows primary monitor even when another monitor is positioned left/above it.
//!
//! Run on Windows:
//!   `cargo run -p platform-win --example win_injector_display_demo`

#[cfg(target_os = "windows")]
use windows::Win32::Foundation::{LPARAM, RECT};
#[cfg(target_os = "windows")]
use windows::Win32::Graphics::Gdi::{
    EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFO,
};

#[cfg(target_os = "windows")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Rect {
    left: i32,
    top: i32,
    width: i32,
    height: i32,
}

#[cfg(target_os = "windows")]
impl Rect {
    fn from_rect(rect: RECT) -> Option<Self> {
        let width = rect.right - rect.left;
        let height = rect.bottom - rect.top;
        (width > 0 && height > 0).then_some(Self {
            left: rect.left,
            top: rect.top,
            width,
            height,
        })
    }
}

#[cfg(target_os = "windows")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Monitor {
    rect: Rect,
    primary: bool,
}

#[cfg(target_os = "windows")]
unsafe extern "system" fn enum_monitor(
    monitor: HMONITOR,
    _hdc: HDC,
    _rect: *mut RECT,
    data: LPARAM,
) -> windows::core::BOOL {
    const MONITORINFOF_PRIMARY: u32 = 0x0000_0001;

    let monitors = unsafe { &mut *(data.0 as *mut Vec<Monitor>) };
    let mut info = MONITORINFO {
        cbSize: std::mem::size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    if unsafe { GetMonitorInfoW(monitor, &mut info) }.as_bool() {
        if let Some(rect) = Rect::from_rect(info.rcMonitor) {
            monitors.push(Monitor {
                rect,
                primary: (info.dwFlags & MONITORINFOF_PRIMARY) != 0,
            });
        }
    }
    true.into()
}

#[cfg(target_os = "windows")]
fn win32_monitors() -> Option<Vec<Monitor>> {
    let mut monitors = Vec::new();
    let ok = unsafe {
        EnumDisplayMonitors(
            None,
            None,
            Some(enum_monitor),
            LPARAM((&mut monitors as *mut Vec<Monitor>) as isize),
        )
    };
    ok.as_bool().then_some(monitors)
}

#[cfg(target_os = "windows")]
fn main() {
    use mouser_core::platform::InputInjection;
    use platform_win::{active_display_bounds, cursor_position, WinInjector};

    let displays = match active_display_bounds() {
        Ok(displays) => displays,
        Err(e) => {
            eprintln!("active_display_bounds failed: {e}");
            std::process::exit(2);
        }
    };
    if displays.is_empty() {
        eprintln!("no active displays reported");
        std::process::exit(2);
    }

    println!("active displays after Mouser ordering:");
    for display in &displays {
        println!(
            "  id={} origin=({}, {}) size={}x{}",
            display.id, display.left, display.top, display.width, display.height
        );
    }

    let ordered_zero = displays[0];
    let Some(monitors) = win32_monitors() else {
        eprintln!("could not independently enumerate Win32 monitors");
        std::process::exit(2);
    };
    let Some(primary) = monitors.iter().find(|m| m.primary).map(|m| m.rect) else {
        eprintln!("could not independently resolve Win32 primary monitor");
        std::process::exit(2);
    };
    let regression_layout = monitors
        .iter()
        .any(|m| !m.primary && (m.rect.left < primary.left || m.rect.top < primary.top));
    if !regression_layout {
        eprintln!(
            "RESULT: display_id_0_on_primary=inconclusive; arrange a non-primary monitor left of or above the primary"
        );
        std::process::exit(3);
    }
    println!(
        "Win32 primary monitor: origin=({}, {}) size={}x{}",
        primary.left, primary.top, primary.width, primary.height
    );
    let ordered_zero_matches_primary = ordered_zero.left == primary.left
        && ordered_zero.top == primary.top
        && ordered_zero.width == primary.width
        && ordered_zero.height == primary.height;
    if !ordered_zero_matches_primary {
        eprintln!("RESULT: display_id_0_on_primary=no; ordered display 0 is not Win32 primary");
        std::process::exit(2);
    }

    let target_x = ordered_zero.width / 2;
    let target_y = ordered_zero.height / 2;
    println!("moving via WinInjector::move_cursor(display_id=0, {target_x}, {target_y})");

    let injector = WinInjector::new();
    if let Err(e) = injector.move_cursor(0, target_x, target_y) {
        eprintln!("WinInjector move_cursor failed: {e}");
        std::process::exit(2);
    }

    std::thread::sleep(std::time::Duration::from_millis(150));
    match cursor_position() {
        Ok(pos) => {
            println!("cursor after move: ({}, {})", pos.x, pos.y);
            let expected_x = primary.left + target_x;
            let expected_y = primary.top + target_y;
            let near_expected = (pos.x - expected_x).abs() <= 2 && (pos.y - expected_y).abs() <= 2;
            if near_expected {
                println!("RESULT: display_id_0_on_primary=yes");
            } else {
                eprintln!(
                    "RESULT: display_id_0_on_primary=no; cursor is not near primary center ({expected_x}, {expected_y})"
                );
                std::process::exit(2);
            }
        }
        Err(e) => {
            eprintln!("cursor_position failed: {e}");
            std::process::exit(2);
        }
    }
}

#[cfg(not(target_os = "windows"))]
fn main() {
    eprintln!("win_injector_display_demo is Windows-only; nothing to do on this host.");
}
