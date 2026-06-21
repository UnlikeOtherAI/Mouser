import SwiftUI

/// Quick device-selection row (brief: "Tap Mac / Windows / Linux — instant
/// ownership transfer"). In the real app each tap issues an `OwnershipRequest`
/// (architecture §9); here it just updates the bound selection.
struct DeviceSelectorRow: View {
    @Binding var selected: Device

    var body: some View {
        HStack(spacing: 10) {
            ForEach(Device.allCases) { device in
                chip(for: device)
            }
        }
        .accessibilityIdentifier("device.selector")
    }

    private func chip(for device: Device) -> some View {
        let isSelected = device == selected
        return Button {
            selected = device
        } label: {
            HStack(spacing: 6) {
                Image(systemName: device.symbolName)
                    .font(.system(size: 14, weight: .semibold))
                Text(device.rawValue)
                    .font(.subheadline.weight(.semibold))
            }
            .frame(maxWidth: .infinity)
            .padding(.vertical, 11)
            .background(
                RoundedRectangle(cornerRadius: 12, style: .continuous)
                    .fill(isSelected ? Color.accentColor : Color(white: 0.18))
            )
            .foregroundStyle(isSelected ? Color.white : Color.white.opacity(0.7))
            .overlay(
                RoundedRectangle(cornerRadius: 12, style: .continuous)
                    .strokeBorder(
                        isSelected ? Color.white.opacity(0.25) : Color.clear,
                        lineWidth: 1
                    )
            )
        }
        .buttonStyle(.plain)
        .accessibilityIdentifier("device.chip.\(device.rawValue)")
    }
}
