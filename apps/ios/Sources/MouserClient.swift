import SwiftUI

/// App-facing wrapper around the uniffi `MobileClient` (the Rust source/controller in
/// `mouser-ffi`). The trackpad emits **relative** deltas, but the engine forwards
/// absolute motion samples between updates (§7.6), so this integrates deltas into a
/// virtual absolute cursor centred in the engine's coordinate span (mirrors the
/// Android `MouserClient`). All senders are no-ops while disconnected, so the gesture
/// layer can call them unconditionally.
@MainActor
final class MouserClient: ObservableObject {
    @Published private(set) var isConnected = false
    @Published private(set) var status = "Not connected"

    private let inner = MobileClient()

    /// The engine clamps absolute coordinates to the peer's real display; we move a
    /// virtual cursor across a large span and let the peer clamp.
    private static let span: Int32 = 1 << 20
    private var cursorX: Int32 = MouserClient.span / 2
    private var cursorY: Int32 = MouserClient.span / 2

    /// This device's own `device_id` (base32), for display/pairing.
    var deviceId: String { inner.deviceId() }

    /// Dial a peer engine (host/port obtained out-of-band; NWBrowser discovery + a
    /// connect UI are a follow-up, matching Android). Surfaces failures into `status`.
    func connect(host: String, port: UInt16, peerId: String) {
        do {
            try inner.connect(host: host, port: port, peerDeviceIdBase32: peerId)
            cursorX = MouserClient.span / 2
            cursorY = MouserClient.span / 2
            isConnected = true
            status = "Controlling \(host)"
        } catch {
            isConnected = false
            status = "Connect failed: \(error.localizedDescription)"
        }
    }

    func disconnect() {
        inner.disconnect()
        isConnected = false
        status = "Not connected"
    }

    // MARK: - Gesture forwarding

    func move(_ delta: CGSize) {
        guard isConnected else { return }
        cursorX = clampSpan(cursorX + i32(delta.width))
        cursorY = clampSpan(cursorY + i32(delta.height))
        inner.sendPointerMoved(displayId: 0, x: cursorX, y: cursorY)
    }

    func scroll(_ delta: CGSize) {
        guard isConnected else { return }
        inner.sendScroll(dx: i32(delta.width), dy: i32(delta.height))
    }

    /// A full press+release of a pointer button (0=left, 1=right, 2=middle).
    func click(_ button: UInt8) {
        guard isConnected else { return }
        inner.sendButton(button: button, down: true)
        inner.sendButton(button: button, down: false)
    }

    func button(_ button: UInt8, down: Bool) {
        guard isConnected else { return }
        inner.sendButton(button: button, down: down)
    }

    func key(usage: UInt16, down: Bool, mods: UInt16 = 0) {
        guard isConnected else { return }
        inner.sendKey(usage: usage, down: down, mods: mods)
    }

    /// Forward a typed character as a HID press+release, if it maps to a key on a
    /// US layout (`HidKeymap`). Unmappable characters are silently skipped.
    func type(_ character: Character) {
        guard isConnected, let stroke = HidKeymap.stroke(for: character) else { return }
        key(usage: stroke.usage, down: true, mods: stroke.mods)
        key(usage: stroke.usage, down: false, mods: stroke.mods)
    }

    /// Forward a single named key (e.g. Return on submit, Backspace on delete) as a
    /// press+release with no modifiers.
    func tapKey(_ usage: UInt16) {
        guard isConnected else { return }
        key(usage: usage, down: true)
        key(usage: usage, down: false)
    }

    // MARK: - Safe conversions

    /// Round + clamp a CGFloat delta to a sane Int32 (guards NaN/∞ → never traps).
    private func i32(_ value: CGFloat) -> Int32 {
        let rounded = value.rounded()
        guard rounded.isFinite else { return 0 }
        return Int32(min(max(rounded, -100_000), 100_000))
    }

    private func clampSpan(_ value: Int32) -> Int32 {
        min(max(value, 0), MouserClient.span - 1)
    }
}
