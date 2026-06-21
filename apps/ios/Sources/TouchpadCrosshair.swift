import SwiftUI

/// Live visualisation of the active finger contacts on the trackpad surface.
///
/// Renders a crosshair-style ring for each touch point reported by
/// `TrackpadHostView`. Two rings means a two-finger gesture (scroll / right-click
/// / pinch), one means a move / click — so a screenshot of an in-progress gesture
/// is self-explanatory. The ring tint follows the current event.
struct TouchpadCrosshair: View {
    let points: [CGPoint]
    var tint: Color = .green

    var body: some View {
        ZStack {
            ForEach(Array(points.enumerated()), id: \.offset) { _, point in
                ring(at: point)
            }
        }
        .allowsHitTesting(false)
    }

    private func ring(at point: CGPoint) -> some View {
        ZStack {
            Circle()
                .fill(tint.opacity(0.18))
                .frame(width: 64, height: 64)
            Circle()
                .strokeBorder(tint.opacity(0.85), lineWidth: 1.5)
                .frame(width: 64, height: 64)
            Circle()
                .fill(tint)
                .frame(width: 10, height: 10)
            Rectangle()
                .fill(tint.opacity(0.6))
                .frame(width: 1, height: 22)
            Rectangle()
                .fill(tint.opacity(0.6))
                .frame(width: 22, height: 1)
        }
        .position(point)
    }
}
