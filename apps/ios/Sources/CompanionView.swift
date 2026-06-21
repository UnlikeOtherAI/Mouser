import SwiftUI

/// Root companion screen (brief: Mobile Companion App).
///
/// Two layouts, switched on orientation via the vertical size class
/// (requirement §1):
///   • Portrait (`.regular` height): the signature split — touchpad above, a thin
///     "Controlling:" banner, the device-selector row, and a capture field that
///     summons the NATIVE iOS keyboard below.
///   • Landscape (`.compact` height): the ENTIRE screen becomes one trackpad
///     (no keyboard, no chrome beyond a small overlay) to maximise the surface.
struct CompanionView: View {
    @State private var selected: Device = .mac
    @State private var captured: String = ""
    @FocusState private var keyboardFocused: Bool
    @Environment(\.verticalSizeClass) private var verticalSizeClass

    /// Landscape on iPhone collapses the height into `.compact`; that is our
    /// signal to go full-screen-trackpad.
    private var isLandscape: Bool { verticalSizeClass == .compact }

    var body: some View {
        Group {
            if isLandscape {
                landscapeLayout
            } else {
                portraitLayout
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .top)
        .background(Color.black.ignoresSafeArea())
        .preferredColorScheme(.dark)
        .onChange(of: isLandscape) { _, nowLandscape in
            // The native keyboard has no place in the full-screen landscape pad;
            // drop focus so it dismisses, and restore it back in portrait.
            keyboardFocused = !nowLandscape
        }
        .onAppear {
            guard !isLandscape else { return }
            // Raise the native keyboard so the portrait split is visible at once.
            DispatchQueue.main.asyncAfter(deadline: .now() + 0.35) {
                keyboardFocused = true
            }
        }
    }

    // MARK: - Portrait (touchpad + native keyboard)

    private var portraitLayout: some View {
        VStack(spacing: 12) {
            TouchpadView(deviceName: selected.rawValue, compact: true)
                .frame(maxWidth: .infinity)
                .layoutPriority(1)

            ControllingBanner(deviceName: selected.rawValue)
            DeviceSelectorRow(selected: $selected)
            captureField
        }
        .padding(.horizontal, 14)
        .padding(.top, 10)
    }

    // MARK: - Landscape (full-screen trackpad)

    private var landscapeLayout: some View {
        // TouchpadView bleeds its own surface to the edges (full-screen trackpad)
        // while keeping the readout/badge inside the safe area, so we do NOT
        // ignore safe area here — that lets the inner GeometryReader still report
        // the notch insets the overlays need.
        TouchpadView(deviceName: selected.rawValue, compact: false)
            .accessibilityIdentifier("landscape.fullpad")
    }

    // MARK: - Capture field (portrait only)

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
