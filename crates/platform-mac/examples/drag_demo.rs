//! `drag_demo` — exercises the macOS cross-machine drag spike (`dragdrop`).
//!
//! It performs three steps and reports each honestly:
//!   1. **Pasteboard round-trip** (fully headless-verifiable): write a temp file's URL
//!      to the general pasteboard as `public.file-url`, then read it back.
//!   2. **Begin a native drag session** for that file at a cursor point. This builds the
//!      overlay window/view/source/`NSDraggingItem` and calls
//!      `-beginDraggingSessionWithItems:event:source:`. Whether the session is actually
//!      *tracked* depends on a live GUI/WindowServer session — see the printed notes.
//!   3. Print what was verified vs. what needs a real GUI / TCC.
//!
//! Run: `cargo run -p platform-mac --example drag_demo`
//!
//! Honest limitations (also in `dragdrop`'s module docs):
//! - AppKit drag is main-thread + WindowServer-attached. Over SSH / in headless CI there
//!   is no connected WindowServer, so the begin-drag call may return without a tracked
//!   session, or the process may not be allowed a window at all.
//! - A **fully automated drop into Finder cannot be injected headlessly** — it needs a
//!   GUI login session and, for the remote inject side, TCC Accessibility.

use std::fs;
use std::path::PathBuf;

use objc2::MainThreadMarker;
use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy};
use objc2_foundation::NSPoint;
use platform_mac::{begin_file_drag, read_dragged_file_urls, write_file_urls};

fn main() {
    let Some(mtm) = MainThreadMarker::new() else {
        eprintln!("RESULT: not on the main thread — AppKit drag is main-thread-only.");
        std::process::exit(2);
    };

    // A temp file to drag.
    let path = std::env::temp_dir().join(format!("mouser-drag-demo-{}.txt", std::process::id()));
    if let Err(e) = fs::write(&path, b"Mouser cross-machine drag demo payload\n") {
        eprintln!("RESULT: could not create temp file: {e}");
        std::process::exit(2);
    }
    println!("temp file: {}", path.display());

    // --- Step 1: pasteboard round-trip (public.file-url) — headless-verifiable. ---
    let wrote = write_file_urls(std::slice::from_ref(&path));
    let read_back: Vec<PathBuf> = read_dragged_file_urls();
    let pasteboard_ok = wrote && read_back.iter().any(|p| p == &path);
    println!(
        "pasteboard round-trip: wrote={wrote} read_back={:?} -> {}",
        read_back,
        if pasteboard_ok { "OK" } else { "FAILED" }
    );

    // NSApplication must exist before we create a window. Accessory policy keeps it out
    // of the Dock; it still needs a GUI session to actually display/track a drag.
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);

    // --- Step 2: begin a native drag session under a cursor point. ---
    let cursor = NSPoint::new(400.0, 400.0);
    match begin_file_drag(mtm, std::slice::from_ref(&path), cursor) {
        Ok(session) => {
            let seq = session.sequence_number();
            println!("begin_file_drag: session started, draggingSequenceNumber={seq}");
            if seq != 0 {
                println!("  -> WindowServer assigned a real session sequence number.");
            } else {
                println!(
                    "  -> sequence number 0: the call path ran but no tracked session was \
                     created (expected without a connected GUI/WindowServer)."
                );
            }
        }
        Err(e) => println!("begin_file_drag: call path reachable but returned: {e}"),
    }

    // --- Step 3: honest summary. ---
    println!("---");
    println!("VERIFIED HEADLESSLY:");
    println!(
        "  - public.file-url pasteboard write+read: {}",
        if pasteboard_ok { "yes" } else { "no" }
    );
    println!("  - overlay NSWindow/NSView + NSDraggingItem + NSDraggingSource built: yes (compiled & called)");
    println!("  - begin-drag call path reachable on the main thread: yes");
    println!("NEEDS A REAL GUI / TCC (NOT injectable headlessly):");
    println!(
        "  - the WindowServer actually TRACKING the drag and a DROP landing in Finder/desktop"
    );
    println!(
        "  - a held hardware mouse button behind the session (synthetic NSEvent is not enough)"
    );
    println!("  - on the cross-machine inject side, TCC Accessibility for the receiving process");

    let _ = fs::remove_file(&path);

    if pasteboard_ok {
        println!("RESULT: drag spike OK (pasteboard verified; native session call path exercised)");
    } else {
        eprintln!("RESULT: pasteboard round-trip failed — see output above");
        std::process::exit(2);
    }
}
