//! Demo: create a virtual pointer + keyboard via /dev/uinput, emit a few
//! relative moves, click, and a key press, then exit cleanly.
//!
//! Run on Linux (needs access to `/dev/uinput`):
//!     cargo run -p platform-linux --example uinput_demo
//! If `/dev/uinput` is root-only, run with `sudo -E` or add yourself to the
//! `input` group (and `sudo modprobe uinput`).

#[cfg(target_os = "linux")]
fn main() -> std::io::Result<()> {
    use std::thread::sleep;
    use std::time::Duration;

    use platform_linux::{Button, Key, VirtualDevice, DEVICE_NAME};

    println!("[uinput_demo] opening /dev/uinput and creating '{DEVICE_NAME}'...");
    let dev = VirtualDevice::new()?;

    // Give udev a moment to settle so the node is observable by inspectors.
    sleep(Duration::from_millis(300));

    match dev.sys_path() {
        Ok(p) => println!("[uinput_demo] kernel sys path: {}", p.display()),
        Err(e) => println!("[uinput_demo] sys_path unavailable: {e}"),
    }

    println!("[uinput_demo] emitting relative moves...");
    for _ in 0..10 {
        dev.move_rel(5, 3)?;
        sleep(Duration::from_millis(20));
    }
    dev.move_rel(-20, -10)?;

    println!("[uinput_demo] left click...");
    dev.button(Button::Left, true)?;
    sleep(Duration::from_millis(40));
    dev.button(Button::Left, false)?;

    println!("[uinput_demo] typing 'A'...");
    dev.key(Key::A, true)?;
    sleep(Duration::from_millis(40));
    dev.key(Key::A, false)?;

    // Hold the device a moment so external inspectors can see it.
    sleep(Duration::from_millis(500));

    println!("[uinput_demo] done; destroying device on drop.");
    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("uinput_demo is Linux-only; nothing to do on this host.");
}
