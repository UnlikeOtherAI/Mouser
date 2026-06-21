//! `inject_demo` — drives the cursor through a visible square on the virtual
//! desktop via `SendInput`, then types the letters "hi" (HID usages) into
//! whatever window has focus.
//!
//! It reads the cursor position before and after (`GetCursorPos`, see
//! `inject::cursor_position`) and reports whether the cursor actually moved —
//! proof of injection, or evidence that something (e.g. injecting into an
//! elevated window under UIPI, or the secure desktop) blocked it.
//!
//! ACCEPTANCE TEST (run on a Windows box — see `docs/windows-build.md`):
//!   1. Open Notepad and click into it so it has focus.
//!   2. `cargo run -p platform-win --example inject_demo`
//!   3. Expect the cursor to trace a square AND "hi" to appear in Notepad.
//!
//! Run: `cargo run -p platform-win --example inject_demo`

#[cfg(target_os = "windows")]
fn main() {
    use std::thread::sleep;
    use std::time::Duration;

    use platform_win::{cursor_position, key, move_cursor};
    use windows::Win32::UI::WindowsAndMessaging::{
        GetSystemMetrics, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN,
        SM_YVIRTUALSCREEN,
    };

    // SAFETY: GetSystemMetrics is a pure read of system constants.
    let (vx, vy, vw, vh) = unsafe {
        (
            GetSystemMetrics(SM_XVIRTUALSCREEN),
            GetSystemMetrics(SM_YVIRTUALSCREEN),
            GetSystemMetrics(SM_CXVIRTUALSCREEN),
            GetSystemMetrics(SM_CYVIRTUALSCREEN),
        )
    };
    println!("virtual desktop: origin=({vx},{vy}) size={vw}x{vh}");

    let before = cursor_position();
    println!("cursor BEFORE: {before:?}");

    // Inset square corners so we stay well inside the visible desktop.
    let inset = (vw.min(vh) / 5).max(80);
    let left = vx + inset;
    let right = vx + vw - inset;
    let top = vy + inset;
    let bottom = vy + vh - inset;
    let corners = [(left, top), (right, top), (right, bottom), (left, bottom)];
    println!("tracing square (inset {inset}px) through 4 corners:");
    for (i, (x, y)) in corners.iter().enumerate() {
        match move_cursor(*x, *y) {
            Ok(()) => println!(
                "  corner {}: move_cursor({x},{y}) -> now {:?}",
                i + 1,
                cursor_position()
            ),
            Err(e) => println!("  corner {}: move_cursor failed: {e}", i + 1),
        }
        sleep(Duration::from_millis(300));
    }

    // Type "hi" into the focused window: HID usage 0x0B = h, 0x0C = i.
    println!("typing 'hi' into the focused window (focus Notepad first)...");
    for usage in [0x0Bu16, 0x0C] {
        if let Err(e) = key(usage, true) {
            println!("  key down {usage:#04x} failed: {e}");
        }
        sleep(Duration::from_millis(30));
        if let Err(e) = key(usage, false) {
            println!("  key up {usage:#04x} failed: {e}");
        }
        sleep(Duration::from_millis(60));
    }

    sleep(Duration::from_millis(150));
    let after = cursor_position();
    println!("cursor AFTER:  {after:?}");

    let moved = match (&before, &after) {
        (Ok(b), Ok(a)) => (a.x - b.x).abs() > 1 || (a.y - b.y).abs() > 1,
        _ => false,
    };

    if moved {
        println!("RESULT: cursor_moved=yes");
    } else {
        eprintln!(
            "RESULT: cursor_moved=no.\n\
             If you expected motion: the target window may be elevated (UIPI \
             blocks injection from a non-elevated, non-uiAccess process), or you \
             are on the UAC secure desktop / lock screen (a separate desktop an \
             ordinary process cannot reach). See docs/windows-build.md."
        );
        std::process::exit(2);
    }
}

#[cfg(not(target_os = "windows"))]
fn main() {
    eprintln!("inject_demo is Windows-only; nothing to do on this host.");
}
