import SwiftUI

/// Root companion screen (brief: Mobile Companion App).
///
/// Portrait, single screen split into stacked areas:
///   - top ~60%: the remote touchpad surface,
///   - a thin "Controlling: <device>" banner,
///   - the quick device-selector row (Mac / Windows / Linux),
///   - bottom: a focused capture field that summons the NATIVE iOS keyboard
///     (its keystrokes will later become HID `KeyEvent`s, architecture §9).
struct CompanionView: View {
    @State private var selected: Device = .mac
    @State private var captured: String = ""
    @FocusState private var keyboardFocused: Bool

    var body: some View {
        VStack(spacing: 12) {
            TouchpadView(deviceName: selected.rawValue)
                // Top ~60% of the safe area.
                .frame(maxWidth: .infinity)
                .layoutPriority(1)

            ControllingBanner(deviceName: selected.rawValue)

            DeviceSelectorRow(selected: $selected)

            captureField
        }
        .padding(.horizontal, 14)
        .padding(.top, 10)
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .top)
        .background(Color.black.ignoresSafeArea())
        .preferredColorScheme(.dark)
        .onAppear {
            // Raise the native keyboard so the signature split layout is visible
            // immediately (and so the screenshot proves the keyboard renders).
            DispatchQueue.main.asyncAfter(deadline: .now() + 0.35) {
                keyboardFocused = true
            }
        }
    }

    /// The bottom capture field. It is intentionally minimal: its only job in
    /// this spike is to hold keyboard focus so the system keyboard appears.
    private var captureField: some View {
        HStack(spacing: 8) {
            Image(systemName: "keyboard")
                .foregroundStyle(.white.opacity(0.5))
            TextField("Type to send keystrokes…", text: $captured)
                .textFieldStyle(.plain)
                .foregroundStyle(.white)
                .autocorrectionDisabled()
                .textInputAutocapitalization(.never)
                .focused($keyboardFocused)
                .submitLabel(.send)
                .accessibilityIdentifier("keyboard.field")
        }
        .padding(.horizontal, 14)
        .padding(.vertical, 12)
        .background(
            RoundedRectangle(cornerRadius: 12, style: .continuous)
                .fill(Color(white: 0.14))
        )
    }
}

#Preview {
    CompanionView()
}
