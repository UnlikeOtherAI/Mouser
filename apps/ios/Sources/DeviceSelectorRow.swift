import SwiftUI

/// Quick device-selection row (brief: "Tap a computer — instant ownership
/// transfer"). Each tap selects a discovered peer and `onSelect` dials it through
/// `MouserClient`. With no peers discovered yet it shows a "searching" state
/// instead of fabricated devices.
struct DeviceSelectorRow: View {
    @ObservedObject var store: PeerStore
    /// Invoked when a chip is tapped, after the store records the selection — the
    /// owner uses it to dial the peer (host/port/device_id) via `MouserClient`.
    var onSelect: (Peer) -> Void = { _ in }

    var body: some View {
        Group {
            if store.peers.isEmpty {
                emptyState
            } else {
                HStack(spacing: 10) {
                    ForEach(store.peers) { peer in
                        chip(for: peer)
                    }
                }
            }
        }
        .accessibilityIdentifier("device.selector")
    }

    private var emptyState: some View {
        HStack(spacing: 8) {
            ProgressView()
                .controlSize(.small)
                .tint(.white.opacity(0.6))
            Text("Searching for computers on your Wi-Fi…")
                .font(.subheadline)
                .foregroundStyle(.white.opacity(0.6))
            Spacer(minLength: 0)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(.vertical, 11)
        .padding(.horizontal, 12)
        .background(
            RoundedRectangle(cornerRadius: 12, style: .continuous)
                .fill(Color(white: 0.14))
        )
        .accessibilityIdentifier("device.selector.empty")
        .accessibilityLabel("Searching for computers on your network")
    }

    private func chip(for peer: Peer) -> some View {
        let isSelected = peer.id == store.selected?.id
        return Button {
            store.select(peer)
            onSelect(peer)
        } label: {
            HStack(spacing: 6) {
                Image(systemName: peer.kind.symbolName)
                    .font(.system(size: 14, weight: .semibold))
                Text(peer.name)
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
        .accessibilityIdentifier("device.chip.\(peer.name)")
    }
}
