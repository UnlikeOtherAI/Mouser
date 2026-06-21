import SwiftUI

/// The kind of the most-recent discrete trackpad event, for the live readout.
enum TrackpadEventKind: String {
    case none = "—"
    case move = "MOVE"
    case leftClick = "LEFT CLICK"
    case rightClick = "RIGHT CLICK"
    case scroll = "SCROLL"
    case momentum = "MOMENTUM"
    case dragSelect = "CLICK-DRAG"
    case magnify = "MAGNIFY"
    case rotate = "ROTATE"
    case forceClick = "FORCE CLICK"

    /// Colour used to surface this event in the readout, so a screenshot makes
    /// the behaviour obvious.
    var tint: Color {
        switch self {
        case .none: return .white.opacity(0.5)
        case .move, .scroll, .momentum: return .green
        case .leftClick, .dragSelect: return .cyan
        case .rightClick: return .orange
        case .magnify, .rotate: return .purple
        case .forceClick: return .pink
        }
    }
}

/// Single source of truth for the trackpad's live state (requirement §2:
/// "show a small live readout … so behavior is verifiable in a screenshot").
///
/// `TrackpadSurface` (the UIKit gesture layer) pushes updates here; both the
/// portrait and landscape SwiftUI layouts render from it. Keeping one observable
/// means the two orientations cannot drift. Everything published here is exactly
/// what will later be serialised into motion/scroll/click datagrams (architecture
/// §6 / §9) once mouser-ffi is wired in.
@MainActor
final class TrackpadState: ObservableObject {
    // Cursor movement (accelerated, relative).
    @Published var moveDelta: CGSize = .zero
    @Published var cumulativeMove: CGSize = .zero

    // Two-finger scroll (live finger delta or momentum-decayed delta).
    @Published var scrollDelta: CGSize = .zero
    @Published var isMomentum = false

    // Discrete events.
    @Published var lastEvent: TrackpadEventKind = .none
    @Published var clickCount = 0
    @Published var rightClickCount = 0
    @Published var scrollDetentCount = 0

    // Advanced (marked as such in the UI): pinch + rotate.
    @Published var magnification: CGFloat = 1.0
    @Published var rotationDegrees: CGFloat = 0

    // Pressure / Force Touch (requirement §4).
    @Published var forceSupported = false
    @Published var force: CGFloat = 0          // 0…1 normalised
    @Published var isForceClick = false

    // Live touch visualisation.
    @Published var activeTouchPoints: [CGPoint] = []
    @Published var isClickDragging = false

    /// Drives a brief flash when a discrete event fires, so screenshots taken
    /// just after a tap clearly show which event happened.
    @Published var eventFlash = false

    func report(_ kind: TrackpadEventKind) {
        lastEvent = kind
        eventFlash = true
        // Decay the flash; cosmetic only.
        Task { @MainActor in
            try? await Task.sleep(nanoseconds: 350_000_000)
            eventFlash = false
        }
    }

    func registerMove(_ delta: CGSize) {
        moveDelta = delta
        cumulativeMove.width += delta.width
        cumulativeMove.height += delta.height
        lastEvent = .move
    }

    func registerScroll(_ delta: CGSize, momentum: Bool) {
        scrollDelta = delta
        isMomentum = momentum
        lastEvent = momentum ? .momentum : .scroll
    }

    func resetTransient() {
        moveDelta = .zero
        scrollDelta = .zero
    }
}
