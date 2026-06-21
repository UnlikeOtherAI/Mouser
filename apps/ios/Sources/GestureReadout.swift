import SwiftUI

/// Live readout of the trackpad's recognised gestures (requirement §2: "show a
/// small live readout … so behavior is verifiable in a screenshot"). Shared by
/// both the portrait and landscape layouts so they always agree.
///
/// `compact` trims the panel for the portrait layout (where vertical space is
/// shared with the keyboard); the full panel is used in landscape where the whole
/// screen is the trackpad.
struct GestureReadout: View {
    @ObservedObject var state: TrackpadState
    var compact: Bool = false

    var body: some View {
        VStack(alignment: .leading, spacing: compact ? 4 : 7) {
            header
            row("Δ move", value: format(state.moveDelta), tint: .green)
            row("Σ move", value: format(state.cumulativeMove), tint: .white.opacity(0.7))
            row("scroll", value: format(state.scrollDelta),
                tint: state.isMomentum ? .mint : .green,
                trailing: state.isMomentum ? "momentum" : nil)
            if !compact {
                row("detents", value: "\(state.scrollDetentCount)", tint: .white.opacity(0.7))
                counts
                advanced
            }
            forceRow
        }
        .padding(compact ? 10 : 14)
        .background(
            RoundedRectangle(cornerRadius: 14, style: .continuous)
                .fill(Color.black.opacity(0.55))
                .overlay(
                    RoundedRectangle(cornerRadius: 14, style: .continuous)
                        .strokeBorder(Color.white.opacity(0.12), lineWidth: 1)
                )
        )
        .accessibilityIdentifier("touchpad.readout")
        .accessibilityElement(children: .combine)
    }

    private var header: some View {
        HStack(spacing: 8) {
            Text("TRACKPAD")
                .font(.caption2.weight(.bold))
                .tracking(1.8)
                .foregroundStyle(.white.opacity(0.5))
            Spacer(minLength: 8)
            Text(state.lastEvent.rawValue)
                .font(.system(.caption, design: .monospaced).weight(.bold))
                .foregroundStyle(state.lastEvent.tint)
                .padding(.horizontal, 8)
                .padding(.vertical, 3)
                .background(
                    Capsule().fill(state.lastEvent.tint.opacity(state.eventFlash ? 0.28 : 0.12))
                )
                .accessibilityIdentifier("touchpad.lastEvent")
        }
    }

    private var counts: some View {
        HStack(spacing: 14) {
            tally("L", state.clickCount, .cyan)
            tally("R", state.rightClickCount, .orange)
            if state.isClickDragging {
                Text("DRAGGING")
                    .font(.caption2.weight(.bold))
                    .foregroundStyle(.cyan)
            }
        }
    }

    @ViewBuilder
    private var advanced: some View {
        VStack(alignment: .leading, spacing: 4) {
            Text("ADVANCED")
                .font(.system(size: 9, weight: .bold))
                .tracking(1.5)
                .foregroundStyle(.purple.opacity(0.8))
            row("magnify", value: String(format: "%.2f×", state.magnification), tint: .purple)
            row("rotate", value: String(format: "%+.0f°", state.rotationDegrees), tint: .purple)
        }
        .padding(.top, 2)
    }

    @ViewBuilder
    private var forceRow: some View {
        if state.forceSupported {
            row("force",
                value: String(format: "%.0f%%", state.force * 100),
                tint: state.isForceClick ? .pink : .white.opacity(0.7),
                trailing: state.isForceClick ? "force-click" : nil)
                .accessibilityIdentifier("touchpad.force")
        } else if !compact {
            HStack(spacing: 6) {
                Image(systemName: "hand.tap")
                    .font(.system(size: 10))
                Text("force unavailable")
                    .font(.system(size: 10, design: .monospaced))
            }
            .foregroundStyle(.white.opacity(0.35))
            .accessibilityIdentifier("touchpad.force.unavailable")
        }
    }

    // MARK: - Pieces

    private func row(_ label: String, value: String, tint: Color, trailing: String? = nil) -> some View {
        HStack(spacing: 8) {
            Text(label)
                .font(.system(size: 11, design: .monospaced))
                .foregroundStyle(.white.opacity(0.5))
                .frame(width: compact ? 52 : 60, alignment: .leading)
            Text(value)
                .font(.system(.footnote, design: .monospaced).weight(.semibold))
                .foregroundStyle(tint)
            if let trailing {
                Text(trailing)
                    .font(.system(size: 9, weight: .bold))
                    .foregroundStyle(tint)
                    .padding(.horizontal, 6)
                    .padding(.vertical, 2)
                    .background(Capsule().fill(tint.opacity(0.18)))
            }
            Spacer(minLength: 0)
        }
    }

    private func tally(_ label: String, _ count: Int, _ tint: Color) -> some View {
        HStack(spacing: 4) {
            Text(label)
                .font(.system(size: 11, weight: .bold, design: .monospaced))
                .foregroundStyle(tint)
            Text("\(count)")
                .font(.system(.footnote, design: .monospaced).weight(.semibold))
                .foregroundStyle(.white.opacity(0.85))
        }
    }

    private func format(_ size: CGSize) -> String {
        String(format: "%+.0f, %+.0f", size.width, size.height)
    }
}
