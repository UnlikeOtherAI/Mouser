import Combine
import Foundation

/// Local view-model + value types for the clipboard UI hooks (audit R2 — clipboard
/// UI). These mirror the Rust `mouser-clipboard` crate one-for-one so the views can
/// be wired straight onto the engine once the FFI lands; no networking happens here.
///
/// Maps to:
///   • `ClipboardSettings` (settings.rs) ← `ClipboardSyncSettings`
///   • `SyncDirection`      (settings.rs) ← `SyncDirection`
///   • `Progress`           (reassembly.rs) ← `ClipboardTransfer`
/// The defaults match the crate's `ClipboardSettings::default()` (spec §7.7).

/// Which way clipboard content may flow for this device (mirrors
/// `mouser_clipboard::SyncDirection`, §7.7 `direction`).
enum SyncDirection: String, CaseIterable, Identifiable {
    case bidirectional = "Bidirectional"
    case sendOnly = "Send only"
    case receiveOnly = "Receive only"

    var id: String { rawValue }

    /// Maps to `SyncDirection::allows_send`.
    var allowsSend: Bool {
        self == .bidirectional || self == .sendOnly
    }

    /// Maps to `SyncDirection::allows_receive`.
    var allowsReceive: Bool {
        self == .bidirectional || self == .receiveOnly
    }
}

/// The clipboard section of a device's settings (mirrors
/// `mouser_clipboard::ClipboardSettings`, §7.7). All fields are local policy,
/// replicated per device — not cluster-wide. Once the FFI lands these flow
/// straight into `ClipboardEngine::set_settings`.
struct ClipboardSyncSettings: Equatable {
    /// Master on/off. When false: no offer is sent and inbound offers are ignored.
    var sharedClipboard: Bool = true
    /// Per-format gate: `utf8_text` / `html` / `rtf`.
    var syncText: Bool = true
    /// Per-format gate: `png` images.
    var syncImages: Bool = true
    /// Per-format gate: `uri_list` (file references).
    var syncFiles: Bool = true
    /// Skip eager auto-pull above this many bytes (0 = unlimited). Mirrors
    /// `max_auto_sync_bytes`; the UI edits it in MB for legibility.
    var maxAutoSyncBytes: UInt64 = 0
    /// Prefer the OS Universal Clipboard between two Apple devices (default on).
    var preferNativeApple: Bool = true
    /// Direction the clipboard may flow for this device.
    var direction: SyncDirection = .bidirectional

    /// Mirrors `ClipboardSettings::can_offer`.
    var canOffer: Bool { sharedClipboard && direction.allowsSend }
    /// Mirrors `ClipboardSettings::can_receive`.
    var canReceive: Bool { sharedClipboard && direction.allowsReceive }
}

/// A clipboard representation format (mirrors `mouser_protocol::ClipFormat`, used
/// only for the indicator's label here).
enum ClipFormat: String {
    case text = "Text"
    case html = "HTML"
    case rtf = "RTF"
    case png = "Image"
    case files = "Files"
}

/// An in-flight inbound clipboard pull, for the Mac-style "Pasting from <device>…"
/// indicator (mirrors `mouser_clipboard::Progress`, §7.7 wait indicator). `peer`
/// is the originating device; `receivedBytes`/`size` come from the reassembly
/// progress.
struct ClipboardTransfer: Equatable, Identifiable {
    let id = UUID()
    var peer: String
    var format: ClipFormat
    var receivedBytes: UInt64
    var size: UInt64

    /// Mirrors `Progress::fraction` — clamped to [0, 1]; a zero-size payload is
    /// reported complete so an empty clipboard never shows a stuck bar.
    var fraction: Double {
        guard size > 0 else { return 1.0 }
        return min(Double(receivedBytes) / Double(size), 1.0)
    }

    /// Mirrors `Progress::is_complete`.
    var isComplete: Bool { receivedBytes >= size }
}

/// View-model backing the clipboard UI. Holds the editable settings and the
/// current in-flight transfer. `transfer` stays `nil` until a real inbound pull
/// arrives — once `mouser-ffi` lands, `settings` writes route to
/// `ClipboardEngine::set_settings` and `transfer` is fed from
/// `engine.progress(hash)`. No fabricated/demo transfers.
@MainActor
final class ClipboardModel: ObservableObject {
    @Published var settings = ClipboardSyncSettings()
    /// The current in-flight inbound transfer, if any (drives the wait indicator).
    @Published private(set) var transfer: ClipboardTransfer?

    /// Feed a progress update from the engine (single seam for real transfers).
    func updateTransfer(_ transfer: ClipboardTransfer?) {
        self.transfer = transfer
    }

    /// Clear the indicator (mirrors a failed/aborted pull clearing pending state).
    func clearTransfer() {
        transfer = nil
    }
}
