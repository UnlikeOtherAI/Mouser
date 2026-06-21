import SwiftUI

/// Live crosshair drawn at the current touch point on the touchpad surface.
struct TouchpadCrosshair: View {
    let point: CGPoint

    var body: some View {
        ZStack {
            Circle()
                .fill(Color.green.opacity(0.18))
                .frame(width: 64, height: 64)
            Circle()
                .strokeBorder(Color.green.opacity(0.8), lineWidth: 1.5)
                .frame(width: 64, height: 64)
            Circle()
                .fill(Color.green)
                .frame(width: 10, height: 10)
            Rectangle()
                .fill(Color.green.opacity(0.6))
                .frame(width: 1, height: 22)
            Rectangle()
                .fill(Color.green.opacity(0.6))
                .frame(width: 22, height: 1)
        }
        .position(point)
    }
}
