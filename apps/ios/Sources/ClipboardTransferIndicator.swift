import SwiftUI

/// Mac-style clipboard progress / "wait" indicator (audit R2 — clipboard UI),
/// mirroring the §7.7 wait indicator fed by `mouser_clipboard::Progress`.
///
/// While a large representation (`png` / over the control cap) streams over the
/// bulk plane, the receiver shows "Pasting from <device>…" with the peer and a
/// percentage until `last` arrives and the hash verifies. A paste attempt would
/// block on this rather than pasting partial bytes. Driven by a
/// `ClipboardTransfer` (mock today); once the FFI lands it is fed from
/// `engine.progress(hash)` per inbound `ClipboardData`.
struct ClipboardTransferIndicator: View {
    let transfer: ClipboardTransfer

    private var percent: Int { Int((transfer.fraction * 100).rounded()) }

    var body: some View {
        HStack(spacing: 12) {
            icon
            VStack(alignment: .leading, spacing: 6) {
                HStack(spacing: 6) {
                    Text(transfer.isComplete ? "Pasted from " : "Pasting from ")
                        .foregroundStyle(.white.opacity(0.7))
                    + Text(transfer.peer)
                        .foregroundStyle(.white)
                        .bold()
                    Spacer(minLength: 8)
                    Text("\(percent)%")
                        .font(.system(.footnote, design: .monospaced).weight(.semibold))
                        .foregroundStyle(transfer.isComplete ? .green : .white.opacity(0.8))
                        .accessibilityIdentifier("clipboard.transfer.percent")
                }
                .font(.footnote)
                progressBar
            }
        }
        .padding(.horizontal, 14)
        .padding(.vertical, 12)
        .background(
            RoundedRectangle(cornerRadius: 14, style: .continuous)
                .fill(Color(white: 0.13))
                .overlay(
                    RoundedRectangle(cornerRadius: 14, style: .continuous)
                        .strokeBorder(Color.white.opacity(0.10), lineWidth: 1)
                )
        )
        .accessibilityIdentifier("clipboard.transfer")
        .accessibilityElement(children: .combine)
        .accessibilityLabel("\(transfer.isComplete ? "Pasted" : "Pasting") from \(transfer.peer), \(percent) percent")
    }

    private var icon: some View {
        ZStack {
            Circle()
                .fill(Color.green.opacity(0.15))
                .frame(width: 34, height: 34)
            if transfer.isComplete {
                Image(systemName: "checkmark")
                    .font(.system(size: 15, weight: .bold))
                    .foregroundStyle(.green)
            } else {
                Image(systemName: "doc.on.clipboard")
                    .font(.system(size: 14, weight: .semibold))
                    .foregroundStyle(.green)
            }
        }
    }

    /// A determinate Mac-style bar: the offer's `size` gives a known total, so we
    /// render real progress rather than a spinner.
    private var progressBar: some View {
        GeometryReader { geo in
            ZStack(alignment: .leading) {
                Capsule()
                    .fill(Color.white.opacity(0.10))
                Capsule()
                    .fill(transfer.isComplete ? Color.green : Color.green.opacity(0.85))
                    .frame(width: max(geo.size.width * transfer.fraction, 4))
                    .animation(.easeOut(duration: 0.15), value: transfer.fraction)
            }
        }
        .frame(height: 6)
        .accessibilityIdentifier("clipboard.transfer.bar")
    }
}

#Preview("In flight") {
    ClipboardTransferIndicator(
        transfer: ClipboardTransfer(peer: "Mac", format: .png, receivedBytes: 2_400_000, size: 4_200_000)
    )
    .padding()
    .background(Color.black)
}

#Preview("Complete") {
    ClipboardTransferIndicator(
        transfer: ClipboardTransfer(peer: "Windows", format: .png, receivedBytes: 4_200_000, size: 4_200_000)
    )
    .padding()
    .background(Color.black)
}
