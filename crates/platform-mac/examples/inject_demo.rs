//! `inject_demo` — drives the cursor through a visible square on the main
//! display, then performs one no-op left click at the end.
//!
//! It reads the cursor position before and after via Core Graphics
//! (`CGEventGetLocation` of a default event, see `inject::cursor_position`) and
//! reports whether the cursor actually moved — proof of injection, or evidence
//! that macOS blocked it (missing **Accessibility** grant for this process).
//!
//! Run: `cargo run -p platform-mac --example inject_demo`

use std::thread::sleep;
use std::time::Duration;

use platform_mac::{cursor_position, left_click, main_display_bounds, move_cursor};

fn fmt_pos(p: Option<core_graphics::geometry::CGPoint>) -> String {
    match p {
        Some(p) => format!("({:.1}, {:.1})", p.x, p.y),
        None => "<unavailable>".to_string(),
    }
}

fn main() {
    let bounds = main_display_bounds();
    println!(
        "main display: id={} bounds=({:.0},{:.0}) {:.0}x{:.0}",
        bounds.id, bounds.x, bounds.y, bounds.w, bounds.h
    );

    let before = cursor_position();
    println!("cursor BEFORE: {}", fmt_pos(before));

    // Inset square corners so we stay well inside the visible display.
    let inset = (bounds.w.min(bounds.h) * 0.2).max(80.0);
    let corners = bounds.inset_corners(inset);
    println!("tracing square (inset {inset:.0} pt) through 4 corners:");

    for (i, (x, y)) in corners.iter().enumerate() {
        match move_cursor(*x, *y) {
            Ok(()) => println!(
                "  corner {}: move_cursor({x:.1}, {y:.1}) -> now {}",
                i + 1,
                fmt_pos(cursor_position())
            ),
            Err(e) => println!("  corner {}: move_cursor failed: {e}", i + 1),
        }
        sleep(Duration::from_millis(300));
    }

    // No-op click at the final corner (bottom-left).
    let (cx, cy) = corners[3];
    match left_click(cx, cy) {
        Ok(()) => println!("no-op left click at ({cx:.1}, {cy:.1})"),
        Err(e) => println!("left_click failed: {e}"),
    }

    // Settle, then read the final position.
    sleep(Duration::from_millis(150));
    let after = cursor_position();
    println!("cursor AFTER:  {}", fmt_pos(after));

    let moved = match (before, after) {
        (Some(b), Some(a)) => (a.x - b.x).abs() > 0.5 || (a.y - b.y).abs() > 0.5,
        _ => false,
    };

    if moved {
        println!("RESULT: cursor_moved=yes (before {} -> after {})", fmt_pos(before), fmt_pos(after));
    } else {
        eprintln!(
            "RESULT: cursor_moved=no — before {} == after {}.\n\
             If you expected motion: this process lacks the macOS \
             *Accessibility* grant (System Settings -> Privacy & Security -> \
             Accessibility). Posted CGEvents are then silently dropped. The warp \
             path should still move the cursor unless the grant is missing AND \
             the terminal is sandboxed.",
            fmt_pos(before),
            fmt_pos(after)
        );
        std::process::exit(2);
    }
}
