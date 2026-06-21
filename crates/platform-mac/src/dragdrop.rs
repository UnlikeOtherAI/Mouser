//! macOS cross-machine **drag-and-drop** spike (the signature Universal-Control UX).
//!
//! Two halves of the feature live here, both AppKit-driven:
//!
//! - **Receive side** ([`begin_file_drag`]): given a received file path and the cursor
//!   location, start a *native* `NSDraggingSession` under the cursor so the user — who
//!   is still holding the mouse button as the remote cursor crossed onto this machine —
//!   can drop the file on the **desktop, a Finder folder, or any drop target**. We do
//!   this with a transparent, borderless, click-through overlay `NSWindow` + `NSView`,
//!   put the file URL on the dragging pasteboard via an `NSDraggingItem`, and call
//!   `-beginDraggingSessionWithItems:event:source:`.
//! - **Send side** ([`read_dragged_file_urls`]): when the cursor leaves the screen edge
//!   mid-drag, read the general/drag pasteboard for `public.file-url` entries and hand
//!   the path(s) to the transfer engine (`mouser-files`).
//!
//! ## Honest capability reality (see also `docs/tech-stack.md` §4)
//! AppKit drag is **inherently a GUI, main-thread, windowserver-attached** operation:
//! - It must run on the **main thread** (every type here is `MainThreadOnly`); call
//!   from the app's main run loop, not a worker. We take a [`MainThreadMarker`] to make
//!   that a compile-time/`Option` requirement.
//! - `-beginDraggingSessionWithItems:event:source:` needs a **real left-mouse-down
//!   `NSEvent`** still in flight. In a live session the OS-delivered mouse-down is
//!   passed straight through. The spike can *construct* a synthetic mouse-down to prove
//!   the call path, but a synthetic event has no held hardware button behind it, so the
//!   windowserver may decline to actually *track* a drop. A **fully automated drop into
//!   Finder cannot be injected headlessly** — it needs a GUI login session (a connected
//!   WindowServer) and, for the cross-machine inject side, TCC **Accessibility**.
//! - Headlessly we *can* verify: pasteboard read/write of `public.file-url`, building
//!   the overlay window/view/source/`NSDraggingItem`, and that the begin-drag call is
//!   reachable when a `MainThreadMarker` is available — which the `drag_demo` example
//!   reports. What needs a real GUI/TCC is called out there too.

use std::path::{Path, PathBuf};

use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2::{define_class, msg_send, AnyThread, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSBackingStoreType, NSDragOperation, NSDraggingContext, NSDraggingItem, NSDraggingSession,
    NSDraggingSource, NSEvent, NSEventModifierFlags, NSEventType, NSPasteboard,
    NSPasteboardTypeFileURL, NSPasteboardWriting, NSView, NSWindow, NSWindowStyleMask,
};
use objc2_foundation::{NSArray, NSObjectProtocol, NSPoint, NSRect, NSSize, NSString, NSURL};

/// Why beginning a native drag session failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DragError {
    /// Not on the main thread — AppKit drag is main-thread-only (see module docs).
    NotMainThread,
    /// No file paths were supplied to drag.
    NoFiles,
    /// A synthetic `NSEvent` could not be constructed (needed to seed the session).
    EventCreate,
}

impl std::fmt::Display for DragError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotMainThread => write!(f, "drag must be started on the main thread"),
            Self::NoFiles => write!(f, "no files to drag"),
            Self::EventCreate => write!(f, "failed to synthesize the seeding NSEvent"),
        }
    }
}

impl std::error::Error for DragError {}

define_class!(
    // SAFETY: superclass NSObject has no subclassing requirements; no Drop impl; the
    // class is main-thread-only because NSDraggingSource is (MainThreadOnly).
    #[unsafe(super(objc2_foundation::NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "MouserDragSource"]
    #[ivars = ()]
    /// The `NSDraggingSource` for a Mouser-initiated drag. It advertises `Copy` (and
    /// `Generic`) operations so a drop onto Finder/desktop is accepted; nothing else is
    /// needed for the spike.
    struct MouserDragSource;

    unsafe impl NSObjectProtocol for MouserDragSource {}

    unsafe impl NSDraggingSource for MouserDragSource {
        #[unsafe(method(draggingSession:sourceOperationMaskForDraggingContext:))]
        fn source_operation_mask(
            &self,
            _session: &NSDraggingSession,
            _context: NSDraggingContext,
        ) -> NSDragOperation {
            // Allow copying onto the desktop / a Finder folder / any target.
            NSDragOperation::Copy | NSDragOperation::Generic
        }
    }
);

impl MouserDragSource {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(());
        unsafe { msg_send![super(this), init] }
    }
}

/// A live native drag session plus the resources that must outlive it (the overlay
/// window, the source object). Drop ends ownership; the session itself completes on the
/// user's drop or release.
pub struct DragSession {
    _window: Retained<NSWindow>,
    _source: Retained<MouserDragSource>,
    session: Retained<NSDraggingSession>,
}

impl DragSession {
    /// The `draggingSequenceNumber` of the underlying `NSDraggingSession` — a non-zero
    /// id the OS assigns once a session is genuinely created (a cheap success oracle).
    #[must_use]
    pub fn sequence_number(&self) -> isize {
        self.session.draggingSequenceNumber()
    }
}

/// Begin a **native** drag session for `paths`, anchored at `cursor` (global screen
/// coordinates, AppKit's bottom-left-origin space). Returns a [`DragSession`] the caller
/// keeps alive until the OS reports the drop ended.
///
/// Must run on the main thread (`mtm`); see the module docs for the GUI/TCC caveats that
/// govern whether the resulting session is actually *tracked* by the WindowServer.
pub fn begin_file_drag(
    mtm: MainThreadMarker,
    paths: &[PathBuf],
    cursor: NSPoint,
) -> Result<DragSession, DragError> {
    if paths.is_empty() {
        return Err(DragError::NoFiles);
    }

    // A small transparent, borderless, non-activating overlay window centered on the
    // cursor. It is the NSView's host; the user never sees it.
    let frame = NSRect::new(
        NSPoint::new(cursor.x - 1.0, cursor.y - 1.0),
        NSSize::new(2.0, 2.0),
    );
    let window: Retained<NSWindow> = unsafe {
        let alloc = NSWindow::alloc(mtm);
        NSWindow::initWithContentRect_styleMask_backing_defer(
            alloc,
            frame,
            NSWindowStyleMask::Borderless,
            NSBackingStoreType::Buffered,
            false,
        )
    };
    window.setOpaque(false);
    window.setIgnoresMouseEvents(true);
    window.setAlphaValue(0.0);
    let view: Retained<NSView> = window
        .contentView()
        .unwrap_or_else(|| NSView::initWithFrame(NSView::alloc(mtm), frame));

    // Build one NSDraggingItem per file URL on the dragging pasteboard. NSURL conforms
    // to NSPasteboardWriting, so the file URL travels natively (Finder understands it).
    let mut items: Vec<Retained<NSDraggingItem>> = Vec::with_capacity(paths.len());
    for path in paths {
        let url = file_url(path);
        let writer: &ProtocolObject<dyn NSPasteboardWriting> = ProtocolObject::from_ref(&*url);
        let item = NSDraggingItem::initWithPasteboardWriter(NSDraggingItem::alloc(), writer);
        // Give the drag a tiny frame at the cursor (a real app supplies a drag image).
        item.setDraggingFrame(NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(32.0, 32.0)));
        items.push(item);
    }
    let item_refs: Vec<&NSDraggingItem> = items.iter().map(|i| &**i).collect();
    let items_array: Retained<NSArray<NSDraggingItem>> = NSArray::from_slice(&item_refs);

    // Seed the session with a left-mouse-down event at the cursor (see module docs on
    // why a synthetic event proves the path but a held hardware button is what tracks).
    let event = synth_left_mouse_down(cursor).ok_or(DragError::EventCreate)?;
    let source = MouserDragSource::new(mtm);
    let source_proto: &ProtocolObject<dyn NSDraggingSource> = ProtocolObject::from_ref(&*source);

    let session = view.beginDraggingSessionWithItems_event_source(
        &items_array,
        &event,
        source_proto,
    );

    Ok(DragSession {
        _window: window,
        _source: source,
        session,
    })
}

/// Read every `public.file-url` path currently on the **general** pasteboard — the send
/// side's "is a file drag in progress?" probe when the cursor leaves the edge. Returns
/// the local file paths found (in the pasteboard's own ordering).
///
/// `NSPasteboard` is not main-thread-bound, so no marker is required.
#[must_use]
pub fn read_dragged_file_urls() -> Vec<PathBuf> {
    read_file_urls_from(&NSPasteboard::generalPasteboard())
}

/// Read `public.file-url` paths from a specific named drag pasteboard (the live drag
/// pasteboard during an in-flight drag is `NSPasteboardNameDrag`).
#[must_use]
pub fn read_dragged_file_urls_from(pasteboard: &NSPasteboard) -> Vec<PathBuf> {
    read_file_urls_from(pasteboard)
}

fn read_file_urls_from(pasteboard: &NSPasteboard) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Some(items) = pasteboard.pasteboardItems() else {
        return out;
    };
    for item in items.iter() {
        // Each item may expose the file URL as a string under public.file-url.
        if let Some(s) = unsafe { item.stringForType(NSPasteboardTypeFileURL) } {
            if let Some(path) = file_url_string_to_path(&s.to_string()) {
                out.push(path);
            }
        }
    }
    out
}

/// Write `paths` as `public.file-url` items onto the general pasteboard (the send side
/// of a same-machine round-trip, and what the receive side mirrors onto the drag
/// pasteboard). Returns whether the write succeeded.
pub fn write_file_urls(paths: &[PathBuf]) -> bool {
    let pb = NSPasteboard::generalPasteboard();
    pb.clearContents();
    let urls: Vec<Retained<NSURL>> = paths.iter().map(|p| file_url(p)).collect();
    let writers: Vec<&ProtocolObject<dyn NSPasteboardWriting>> = urls
        .iter()
        .map(|u| ProtocolObject::from_ref(&**u))
        .collect();
    let array: Retained<NSArray<ProtocolObject<dyn NSPasteboardWriting>>> =
        NSArray::from_slice(&writers);
    pb.writeObjects(&array)
}

fn file_url(path: &Path) -> Retained<NSURL> {
    let s = NSString::from_str(&path.to_string_lossy());
    NSURL::fileURLWithPath(&s)
}

/// Turn a `file://…` URL string into a local filesystem path (best-effort, no percent
/// decoding beyond what `NSURL` would already have produced via `-path`).
fn file_url_string_to_path(url: &str) -> Option<PathBuf> {
    let s = NSString::from_str(url);
    let nsurl = NSURL::URLWithString(&s)?;
    let path = nsurl.path()?;
    Some(PathBuf::from(path.to_string()))
}

/// Construct a synthetic left-mouse-down `NSEvent` at `location` to seed a drag. See the
/// module docs: this proves the begin-drag *call path* but does not substitute for a
/// real held mouse button (which is what makes the WindowServer track a drop).
fn synth_left_mouse_down(location: NSPoint) -> Option<Retained<NSEvent>> {
    NSEvent::mouseEventWithType_location_modifierFlags_timestamp_windowNumber_context_eventNumber_clickCount_pressure(
        NSEventType::LeftMouseDown,
        location,
        NSEventModifierFlags::empty(),
        0.0,
        0,
        None,
        0,
        1,
        1.0,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_url_roundtrips_through_pasteboard_string_form() {
        // Pure helper: a file URL string parses back to its path. (No AppKit session,
        // so this is safe to run headlessly / off the main thread guard via the URL API.)
        let p = PathBuf::from("/tmp/mouser-drag-test.bin");
        let url = file_url(&p);
        let abs = url.absoluteString().map(|s| s.to_string());
        assert!(abs.as_deref().unwrap_or("").starts_with("file://"));
        let back = file_url_string_to_path(&abs.unwrap()).expect("path back");
        assert_eq!(back, p);
    }
}
