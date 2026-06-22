import SwiftUI

/// Thin persistent status banner (architecture §9), between the touchpad and the
/// device-selector row. Shows "Controlling: <device>" when a peer is selected, or
/// an honest "Not connected" state when none has been discovered yet.
struct ControllingBanner: View {
    /// The controlled device's name, or `nil` when not connected to any peer.
    let deviceName: String?

    private var connected: Bool { deviceName != nil }

    var body: some View {
        HStack(spacing: 8) {
            Circle()
                .fill(connected ? Color.green : Color.gray)
                .frame(width: 8, height: 8)
            if let name = deviceName {
                Text("Controlling: ")
                    .foregroundStyle(.white.opacity(0.6))
                + Text(name)
                    .foregroundStyle(.white)
                    .bold()
            } else {
                Text("Not connected")
                    .foregroundStyle(.white.opacity(0.6))
            }
            Spacer()
            Image(systemName: connected
                ? "dot.radiowaves.left.and.right"
                : "antenna.radiowaves.left.and.right.slash")
                .foregroundStyle(.white.opacity(0.5))
        }
        .font(.footnote)
        .padding(.horizontal, 14)
        .padding(.vertical, 9)
        .background(
            RoundedRectangle(cornerRadius: 10, style: .continuous)
                .fill(Color(white: 0.14))
        )
        .accessibilityIdentifier("controlling.banner")
        .accessibilityElement(children: .combine)
        .accessibilityLabel(deviceName.map { "Controlling \($0)" } ?? "Not connected")
    }
}
