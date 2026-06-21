import SwiftUI

/// Thin persistent "Controlling: <device>" banner (architecture §9), sitting
/// between the touchpad and the device-selector row.
struct ControllingBanner: View {
    let deviceName: String

    var body: some View {
        HStack(spacing: 8) {
            Circle()
                .fill(Color.green)
                .frame(width: 8, height: 8)
            Text("Controlling: ")
                .foregroundStyle(.white.opacity(0.6))
            + Text(deviceName)
                .foregroundStyle(.white)
                .bold()
            Spacer()
            Image(systemName: "dot.radiowaves.left.and.right")
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
        .accessibilityLabel("Controlling \(deviceName)")
    }
}
