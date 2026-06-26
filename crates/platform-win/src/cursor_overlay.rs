//! Software cursor overlay for Windows targets.
//!
//! Some foreground windows legitimately report no native cursor (`GetCursorInfo`
//! shows `hCursor = NULL`, `flags = 0`). VNC clients often draw their own cursor
//! overlay, but the physical console then shows nothing. While Mouser is injecting
//! remote pointer motion, keep a tiny topmost click-through overlay at the injected
//! position so the physical display has an inspectable pointer even when the native
//! cursor is hidden by the foreground app.

#![allow(unsafe_code)] // Win32 overlay window + GDI painting.

use std::sync::mpsc::{self, SyncSender};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::Duration;

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CreateSolidBrush, DeleteObject, EndPaint, FillRect, InvalidateRect, Polygon,
    SelectObject, PAINTSTRUCT,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DispatchMessageW, PeekMessageW, RegisterClassW,
    SetLayeredWindowAttributes, SetWindowPos, ShowWindow, TranslateMessage, CS_HREDRAW, CS_VREDRAW,
    HTTRANSPARENT, HWND_TOPMOST, LWA_COLORKEY, MSG, PM_REMOVE, SWP_NOACTIVATE, SWP_SHOWWINDOW,
    SW_HIDE, SW_SHOWNA, WM_NCHITTEST, WM_PAINT, WNDCLASSW, WS_EX_LAYERED, WS_EX_NOACTIVATE,
    WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_EX_TRANSPARENT, WS_POPUP,
};

const CLASS_NAME: PCWSTR = w!("MouserCursorOverlay");
const SIZE: i32 = 32;
const TRANSPARENT: COLORREF = COLORREF(0x00ff00ff);
const BLACK: COLORREF = COLORREF(0x00000000);
const WHITE: COLORREF = COLORREF(0x00ffffff);

enum OverlayCommand {
    ShowAt { x: i32, y: i32 },
    Hide,
}

struct Overlay {
    pending: Arc<PendingCommand>,
}

type PendingCommand = (Mutex<Option<OverlayCommand>>, Condvar);

static OVERLAY: Mutex<Option<Overlay>> = Mutex::new(None);

pub(crate) fn show_at(x: i32, y: i32) {
    post_show(OverlayCommand::ShowAt { x, y });
}

pub(crate) fn hide() {
    let Ok(overlay) = OVERLAY.lock() else {
        return;
    };
    if let Some(overlay) = overlay.as_ref() {
        overlay.post(OverlayCommand::Hide);
    }
}

fn post_show(command: OverlayCommand) {
    let Ok(mut overlay) = OVERLAY.lock() else {
        return;
    };
    if overlay.is_none() {
        *overlay = spawn_overlay();
    }
    if let Some(overlay) = overlay.as_ref() {
        overlay.post(command);
    }
}

impl Overlay {
    fn post(&self, command: OverlayCommand) {
        let (pending, changed) = &*self.pending;
        if let Ok(mut pending) = pending.lock() {
            *pending = Some(command);
            changed.notify_one();
        }
    }
}

fn spawn_overlay() -> Option<Overlay> {
    let pending = Arc::new((Mutex::new(None), Condvar::new()));
    let thread_pending = Arc::clone(&pending);
    let (ready_tx, ready_rx) = mpsc::sync_channel(1);
    thread::Builder::new()
        .name("mouser-cursor-overlay".into())
        .spawn(move || overlay_thread(thread_pending, ready_tx))
        .ok()?;
    match ready_rx.recv_timeout(Duration::from_secs(2)) {
        Ok(true) => Some(Overlay { pending }),
        _ => None,
    }
}

fn overlay_thread(pending: Arc<PendingCommand>, ready_tx: SyncSender<bool>) {
    let Some(hwnd) = create_overlay_window() else {
        let _ = ready_tx.send(false);
        return;
    };
    let _ = ready_tx.send(true);
    let mut msg = MSG::default();
    loop {
        if let Some(command) = take_command(&pending) {
            apply_command(hwnd, command);
        }
        while unsafe { PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE) }.as_bool() {
            unsafe {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
        thread::sleep(Duration::from_millis(8));
    }
}

fn take_command(pending: &PendingCommand) -> Option<OverlayCommand> {
    let (pending, changed) = pending;
    let Ok(mut command) = pending.lock() else {
        return None;
    };
    if command.is_none() {
        match changed.wait_timeout(command, Duration::from_millis(8)) {
            Ok((guard, _)) => command = guard,
            Err(_) => return None,
        }
    }
    command.take()
}

fn apply_command(hwnd: HWND, command: OverlayCommand) {
    match command {
        OverlayCommand::ShowAt { x, y } => unsafe {
            if let Err(e) = SetWindowPos(
                hwnd,
                Some(HWND_TOPMOST),
                x,
                y,
                SIZE,
                SIZE,
                SWP_NOACTIVATE | SWP_SHOWWINDOW,
            ) {
                eprintln!("mouser: cursor overlay SetWindowPos failed: {e}");
                return;
            }
            let _ = ShowWindow(hwnd, SW_SHOWNA);
            if !InvalidateRect(Some(hwnd), None, true).as_bool() {
                eprintln!("mouser: cursor overlay InvalidateRect failed");
            }
        },
        OverlayCommand::Hide => unsafe {
            let _ = ShowWindow(hwnd, SW_HIDE);
        },
    }
}

fn create_overlay_window() -> Option<HWND> {
    let Ok(hinstance) = (unsafe { GetModuleHandleW(None) }) else {
        eprintln!("mouser: cursor overlay GetModuleHandleW failed");
        return None;
    };
    let class = WNDCLASSW {
        style: CS_HREDRAW | CS_VREDRAW,
        lpfnWndProc: Some(overlay_wndproc),
        hInstance: hinstance.into(),
        lpszClassName: CLASS_NAME,
        ..Default::default()
    };
    let _ = unsafe { RegisterClassW(&class) };
    let hwnd = match unsafe {
        CreateWindowExW(
            WS_EX_LAYERED | WS_EX_TOPMOST | WS_EX_TRANSPARENT | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
            CLASS_NAME,
            w!("mouser-cursor-overlay"),
            WS_POPUP,
            0,
            0,
            SIZE,
            SIZE,
            None,
            None,
            Some(hinstance.into()),
            None,
        )
    } {
        Ok(hwnd) => hwnd,
        Err(e) => {
            eprintln!("mouser: cursor overlay CreateWindowExW failed: {e}");
            return None;
        }
    };
    if hwnd.is_invalid() {
        eprintln!("mouser: cursor overlay CreateWindowExW returned an invalid HWND");
        return None;
    }
    if let Err(e) = unsafe { SetLayeredWindowAttributes(hwnd, TRANSPARENT, 255, LWA_COLORKEY) } {
        eprintln!("mouser: cursor overlay SetLayeredWindowAttributes failed: {e}");
        return None;
    }
    Some(hwnd)
}

unsafe extern "system" fn overlay_wndproc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if msg == WM_NCHITTEST {
        return LRESULT(HTTRANSPARENT as isize);
    }
    if msg == WM_PAINT {
        paint_cursor(hwnd);
        return LRESULT(0);
    }
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

fn paint_cursor(hwnd: HWND) {
    let mut ps = PAINTSTRUCT::default();
    let hdc = unsafe { BeginPaint(hwnd, &mut ps) };
    let bg = unsafe { CreateSolidBrush(TRANSPARENT) };
    if !bg.is_invalid() {
        let rect = RECT {
            left: 0,
            top: 0,
            right: SIZE,
            bottom: SIZE,
        };
        let _ = unsafe { FillRect(hdc, &rect, bg) };
        let _ = unsafe { DeleteObject(bg.into()) };
    }
    draw_polygon(hdc, BLACK, &black_arrow());
    draw_polygon(hdc, WHITE, &white_arrow());
    let _ = unsafe { EndPaint(hwnd, &ps) };
}

fn draw_polygon(hdc: windows::Win32::Graphics::Gdi::HDC, color: COLORREF, points: &[POINT]) {
    let brush = unsafe { CreateSolidBrush(color) };
    if brush.is_invalid() {
        return;
    }
    let old = unsafe { SelectObject(hdc, brush.into()) };
    let _ = unsafe { Polygon(hdc, points) };
    if !old.is_invalid() {
        let _ = unsafe { SelectObject(hdc, old) };
    }
    let _ = unsafe { DeleteObject(brush.into()) };
}

fn black_arrow() -> [POINT; 7] {
    [
        POINT { x: 0, y: 0 },
        POINT { x: 0, y: 23 },
        POINT { x: 7, y: 17 },
        POINT { x: 12, y: 31 },
        POINT { x: 18, y: 29 },
        POINT { x: 13, y: 15 },
        POINT { x: 23, y: 15 },
    ]
}

fn white_arrow() -> [POINT; 7] {
    [
        POINT { x: 2, y: 4 },
        POINT { x: 2, y: 18 },
        POINT { x: 7, y: 13 },
        POINT { x: 13, y: 27 },
        POINT { x: 15, y: 26 },
        POINT { x: 10, y: 12 },
        POINT { x: 17, y: 12 },
    ]
}
