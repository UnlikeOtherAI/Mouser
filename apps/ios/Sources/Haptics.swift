import UIKit

/// Taptic Engine feedback for trackpad events.
///
/// Wraps `UIImpactFeedbackGenerator` / `UISelectionFeedbackGenerator` (requirement
/// §3). These map onto what the real backend will eventually do via mouser-ffi:
/// every click, right-click and scroll-detent the user feels here is a HID event
/// that will later be sent to the active machine (architecture §9). Generators are
/// kept warm with `prepare()` so the latency between gesture and tap is minimal —
/// the trackpad must feel like the real macOS one.
@MainActor
final class Haptics {
    static let shared = Haptics()

    private let lightImpact = UIImpactFeedbackGenerator(style: .light)
    private let mediumImpact = UIImpactFeedbackGenerator(style: .medium)
    private let rigidImpact = UIImpactFeedbackGenerator(style: .rigid)
    private let selection = UISelectionFeedbackGenerator()

    private init() {}

    /// Call when a gesture is starting so the engine is spun up and the first
    /// tap is not delayed.
    func warmUp() {
        lightImpact.prepare()
        mediumImpact.prepare()
        rigidImpact.prepare()
        selection.prepare()
    }

    /// Primary (left) click — a crisp, light tap, like landing a macOS click.
    func leftClick() {
        lightImpact.impactOccurred(intensity: 0.9)
        lightImpact.prepare()
    }

    /// Secondary (right) click — a firmer, distinct tap so it feels different
    /// from a primary click.
    func rightClick() {
        rigidImpact.impactOccurred(intensity: 1.0)
        rigidImpact.prepare()
    }

    /// Beginning of a click-and-drag (selection) — a medium thunk, the "grab".
    func dragStart() {
        mediumImpact.impactOccurred(intensity: 0.8)
        mediumImpact.prepare()
    }

    /// A scroll "detent" crossing — the subtle ratchet a real trackpad gives as
    /// content scrolls. Uses the selection generator (the lightest tick).
    func scrollDetent() {
        selection.selectionChanged()
        selection.prepare()
    }

    /// Force-click threshold crossed (deep press) — only fires on devices with
    /// Force Touch / 3D Touch (requirement §4).
    func forceClick() {
        rigidImpact.impactOccurred(intensity: 1.0)
        rigidImpact.prepare()
    }
}
