import Foundation

/// A target computer the companion can drive.
///
/// In the real app these are discovered cluster peers (architecture §9). For
/// this UI/gesture spike they are a fixed set so the device-selector row and
/// the "Controlling: <device>" banner have something to bind to.
enum Device: String, CaseIterable, Identifiable {
    case mac = "Mac"
    case windows = "Windows"
    case linux = "Linux"

    var id: String { rawValue }

    /// SF Symbol used in the selector chip. Generic glyphs — not OS logos.
    var symbolName: String {
        switch self {
        case .mac: return "laptopcomputer"
        case .windows: return "pc"
        case .linux: return "terminal"
        }
    }
}
