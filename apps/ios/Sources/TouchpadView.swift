import SwiftUI

/// The remote touchpad surface (brief: "Touchpad above"), now a full replica of
/// the macOS trackpad (requirement §2). Composes the UIKit gesture layer
/// (`TrackpadSurface`), a live finger visualisation, and the gesture readout.
///
/// The same view backs both orientations:
///   • portrait  → `compact == true`, sits above the native keyboard,
///   • landscape → `compact == false`, fills the whole screen (one big trackpad).
struct TouchpadView: View {
    /// The controlled device's name, or `nil` when no peer is connected.
    let deviceName: String?
    /// True in portrait (shorter readout, idle hint); false in landscape (full
    /// readout, edge-to-edge surface).
    var compact: Bool = true

    private var connected: Bool { deviceName != nil }

    @StateObject private var state = TrackpadState()
    /// Injected by `CompanionView`; gates streaming and lets the lifecycle stop
    /// momentum when the app backgrounds (audit R2 — lifecycle/reconnect).
    @EnvironmentObject private var lifecycle: AppLifecycle

    var body: some View {
        GeometryReader { geo in
            ZStack {
                // The trackpad surface is full-bleed so the pad is maximised
                // (requirement §1); only the overlays inset for the safe area
                // so the notch / home indicator never clips the readout.
                surface
                grid
                    .ignoresSafeArea(edges: compact ? [] : .all)
                TouchpadCrosshair(points: state.activeTouchPoints, tint: state.lastEvent.tint)
                if state.activeTouchPoints.isEmpty {
                    idleHint
                }
                TrackpadSurface(state: state, lifecycle: lifecycle)
                    .ignoresSafeArea(edges: compact ? [] : .all)
                    .accessibilityIdentifier("touchpad.surface")
                    .accessibilityLabel("Touchpad")
                GestureReadout(state: state, compact: compact)
                    .frame(maxWidth: compact ? .infinity : 340, alignment: .leading)
                    .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
                    .padding(.leading, overlayInset(geo.safeAreaInsets.leading))
                    .padding(.top, overlayInset(geo.safeAreaInsets.top))
                    .padding(.trailing, compact ? 12 : 18)
                if !compact {
                    fullScreenBadge
                        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .bottom)
                        .padding(.bottom, max(geo.safeAreaInsets.bottom, 18))
                }
            }
        }
        .onAppear { Haptics.shared.warmUp() }
    }

    /// Inset overlays by the safe-area amount (with a sensible minimum) so the
    /// readout/badge clear the notch and home indicator while the pad stays
    /// full-bleed.
    private func overlayInset(_ safeArea: CGFloat) -> CGFloat {
        max(safeArea, compact ? 12 : 18)
    }

    // MARK: - Layers

    private var surface: some View {
        let shape = RoundedRectangle(cornerRadius: compact ? 28 : 22, style: .continuous)
        return shape
            .fill(
                LinearGradient(
                    colors: [Color(white: 0.16), Color(white: 0.10)],
                    startPoint: .top, endPoint: .bottom
                )
            )
            .overlay(shape.strokeBorder(Color.white.opacity(0.10), lineWidth: 1))
            // In landscape the pad bleeds edge-to-edge (full-screen trackpad);
            // in portrait it keeps its inset card look above the keyboard.
            .ignoresSafeArea(edges: compact ? [] : .all)
    }

    private var grid: some View {
        Canvas { context, size in
            let step: CGFloat = 36
            var path = Path()
            var x = step
            while x < size.width {
                path.move(to: CGPoint(x: x, y: 0))
                path.addLine(to: CGPoint(x: x, y: size.height))
                x += step
            }
            var y = step
            while y < size.height {
                path.move(to: CGPoint(x: 0, y: y))
                path.addLine(to: CGPoint(x: size.width, y: y))
                y += step
            }
            context.stroke(path, with: .color(.white.opacity(0.04)), lineWidth: 1)
        }
        .allowsHitTesting(false)
    }

    private var idleHint: some View {
        VStack(spacing: 8) {
            Image(systemName: "hand.point.up.left.fill")
                .font(.system(size: compact ? 30 : 38, weight: .light))
                .foregroundStyle(.white.opacity(0.30))
            Text(idleHintText)
                .font(compact ? .subheadline : .headline)
                .multilineTextAlignment(.center)
                .foregroundStyle(.white.opacity(0.38))
        }
        .allowsHitTesting(false)
    }

    /// Idle-hint copy: names the controlled device when connected, otherwise says
    /// so honestly. Landscape keeps the gesture-list hint regardless.
    private var idleHintText: String {
        if !compact {
            return "Full-screen trackpad — drag, tap, scroll, pinch"
        }
        if let name = deviceName {
            return "Drag to move \(name)"
        }
        return "Not connected — drag to preview the trackpad"
    }

    /// Small overlay shown in landscape so the screenshot proves "entire screen
    /// is one touchpad" (requirement §1) while honestly labelling connection state.
    private var fullScreenBadge: some View {
        HStack(spacing: 8) {
            Circle().fill(connected ? Color.green : Color.gray).frame(width: 7, height: 7)
            Text(deviceName.map { "Controlling \($0)" } ?? "Not connected")
                .font(.footnote.weight(.semibold))
                .foregroundStyle(.white.opacity(0.75))
            Text("· full-screen trackpad")
                .font(.footnote)
                .foregroundStyle(.white.opacity(0.45))
        }
        .padding(.horizontal, 14)
        .padding(.vertical, 8)
        .background(Capsule().fill(Color.black.opacity(0.5)))
        .overlay(Capsule().strokeBorder(Color.white.opacity(0.12), lineWidth: 1))
        .allowsHitTesting(false)
        .accessibilityIdentifier("touchpad.fullscreen.badge")
    }
}
