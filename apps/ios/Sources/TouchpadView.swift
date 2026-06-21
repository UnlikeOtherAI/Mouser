import SwiftUI

/// The remote touchpad surface (brief: "Touchpad above").
///
/// Captures drag gestures and renders live finger movement — a crosshair at the
/// current touch point plus a per-frame delta readout. No backend wiring yet;
/// the deltas surfaced here are what will later become motion datagrams
/// (architecture §6 / §9).
struct TouchpadView: View {
    /// Device label shown faintly on the surface, for orientation.
    let deviceName: String

    @State private var touchPoint: CGPoint?
    @State private var lastPoint: CGPoint?
    @State private var delta: CGSize = .zero
    @State private var isTouching = false

    var body: some View {
        GeometryReader { geo in
            ZStack {
                surface
                grid(in: geo.size)
                if isTouching, let point = touchPoint {
                    crosshair(at: point)
                } else {
                    idleHint
                }
                readout
                    .frame(maxWidth: .infinity, maxHeight: .infinity,
                           alignment: .topLeading)
                    .padding(14)
            }
            .contentShape(Rectangle())
            .gesture(dragGesture)
        }
        .accessibilityIdentifier("touchpad.surface")
        .accessibilityLabel("Touchpad")
    }

    // MARK: - Gesture

    private var dragGesture: some Gesture {
        DragGesture(minimumDistance: 0)
            .onChanged { value in
                if let previous = lastPoint {
                    delta = CGSize(
                        width: value.location.x - previous.x,
                        height: value.location.y - previous.y
                    )
                } else {
                    delta = .zero
                }
                lastPoint = value.location
                touchPoint = value.location
                isTouching = true
            }
            .onEnded { _ in
                isTouching = false
                lastPoint = nil
                delta = .zero
            }
    }

    // MARK: - Layers

    private var surface: some View {
        RoundedRectangle(cornerRadius: 28, style: .continuous)
            .fill(
                LinearGradient(
                    colors: [
                        Color(white: 0.16),
                        Color(white: 0.10)
                    ],
                    startPoint: .top,
                    endPoint: .bottom
                )
            )
            .overlay(
                RoundedRectangle(cornerRadius: 28, style: .continuous)
                    .strokeBorder(Color.white.opacity(0.10), lineWidth: 1)
            )
    }

    private func grid(in size: CGSize) -> some View {
        Canvas { context, canvasSize in
            let step: CGFloat = 36
            var path = Path()
            var x: CGFloat = step
            while x < canvasSize.width {
                path.move(to: CGPoint(x: x, y: 0))
                path.addLine(to: CGPoint(x: x, y: canvasSize.height))
                x += step
            }
            var y: CGFloat = step
            while y < canvasSize.height {
                path.move(to: CGPoint(x: 0, y: y))
                path.addLine(to: CGPoint(x: canvasSize.width, y: y))
                y += step
            }
            context.stroke(path, with: .color(.white.opacity(0.04)), lineWidth: 1)
        }
        .allowsHitTesting(false)
    }

    private func crosshair(at point: CGPoint) -> some View {
        TouchpadCrosshair(point: point)
            .allowsHitTesting(false)
    }

    private var idleHint: some View {
        VStack(spacing: 8) {
            Image(systemName: "hand.point.up.left.fill")
                .font(.system(size: 30, weight: .light))
                .foregroundStyle(.white.opacity(0.35))
            Text("Drag to move \(deviceName)")
                .font(.subheadline)
                .foregroundStyle(.white.opacity(0.40))
        }
        .allowsHitTesting(false)
    }

    private var readout: some View {
        VStack(alignment: .leading, spacing: 2) {
            Text("TOUCHPAD")
                .font(.caption2.weight(.semibold))
                .tracking(1.5)
                .foregroundStyle(.white.opacity(0.45))
            Text(String(format: "Δ %+.0f, %+.0f", delta.width, delta.height))
                .font(.system(.footnote, design: .monospaced))
                .foregroundStyle(isTouching ? .green : .white.opacity(0.5))
        }
        .accessibilityIdentifier("touchpad.readout")
    }
}
