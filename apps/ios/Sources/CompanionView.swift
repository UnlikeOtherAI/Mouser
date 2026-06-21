import SwiftUI

/// Root companion screen (brief: Mobile Companion App).
///
/// Two layouts, switched on orientation via the vertical size class
/// (requirement §1):
///   • Portrait (`.regular` height): the signature split — touchpad above, a thin
///     "Controlling:" banner, the device-selector row, and a capture field that
///     summons the NATIVE iOS keyboard BELOW. The split is driven off the live
///     keyboard height (`KeyboardObserver`) so the touchpad fills exactly the
///     space above the keyboard and the capture field sits just above it (audit
///     R2 — keyboard-below layout).
///   • Landscape (`.compact` height): the ENTIRE screen becomes one trackpad
///     (no keyboard, no chrome beyond a small overlay) to maximise the surface.
struct CompanionView: View {
    @State private var selected: Device = .mac
    @State private var captured: String = ""
    @FocusState private var keyboardFocused: Bool
    @Environment(\.verticalSizeClass) private var verticalSizeClass
    @Environment(\.scenePhase) private var scenePhase

    @StateObject private var keyboard = KeyboardObserver()
    @StateObject private var lifecycle = AppLifecycle()
    @StateObject private var clipboard = ClipboardModel()
    @State private var showClipboardSettings = false

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
        .environmentObject(lifecycle)
        // Deterministic focus: the native keyboard belongs in portrait and must be
        // gone in the full-screen landscape pad. Drive focus straight off the
        // orientation — no timed DispatchQueue hack — on appear and on every
        // orientation change.
        .onAppear { keyboardFocused = !isLandscape }
        .onChange(of: isLandscape) { _, nowLandscape in
            keyboardFocused = !nowLandscape
            // Kill any in-flight momentum glide across the orientation switch so a
            // CADisplayLink can't outlive the teardown of the old layout's
            // trackpad surface (audit R2 — momentum on orientation change).
            lifecycle.stopMomentum?()
        }
        // App lifecycle: stop momentum + streaming on background, reconnect on
        // active (audit R2 — lifecycle/reconnect scaffolding).
        .onChange(of: scenePhase) { _, phase in
            lifecycle.handle(phase.asLifecyclePhase)
        }
        // Clipboard settings hook (UI/view-model only; no networking yet).
        .sheet(isPresented: $showClipboardSettings) {
            NavigationStack {
                ClipboardSettingsView(model: clipboard)
                    .toolbar {
                        ToolbarItem(placement: .confirmationAction) {
                            Button("Done") { showClipboardSettings = false }
                        }
                    }
            }
        }
    }

    /// Top chrome shown above the controlling banner in portrait: the live
    /// clipboard transfer indicator (when a pull is in flight) and a button into
    /// clipboard settings. Wires the clipboard UI hooks into the screen.
    @ViewBuilder
    private var clipboardChrome: some View {
        if let transfer = clipboard.transfer {
            ClipboardTransferIndicator(transfer: transfer)
                .transition(.move(edge: .top).combined(with: .opacity))
        }
    }

    private var clipboardButton: some View {
        Button {
            showClipboardSettings = true
        } label: {
            Image(systemName: "doc.on.clipboard")
                .font(.system(size: 14, weight: .semibold))
                .foregroundStyle(.white.opacity(0.7))
                .padding(9)
                .background(
                    RoundedRectangle(cornerRadius: 10, style: .continuous)
                        .fill(Color(white: 0.14))
                )
        }
        .buttonStyle(.plain)
        // Long-press demos the in-flight transfer indicator without a peer
        // (mock); removed once the engine feeds real progress.
        .onLongPressGesture { clipboard.startMockTransfer(peer: selected.rawValue) }
        .accessibilityIdentifier("clipboard.open")
        .accessibilityLabel("Clipboard settings")
    }

    // MARK: - Portrait (touchpad + native keyboard)

    private var portraitLayout: some View {
        // The keyboard occupies `keyboard.height` at the bottom of the window. We
        // reserve that exact band below the chrome so the native keyboard sits
        // BELOW the touchpad rather than floating over it. The touchpad takes all
        // remaining height above.
        VStack(spacing: 12) {
            TouchpadView(deviceName: selected.rawValue, compact: true)
                .frame(maxWidth: .infinity, maxHeight: .infinity)

            clipboardChrome
            HStack(spacing: 10) {
                ControllingBanner(deviceName: selected.rawValue)
                clipboardButton
            }
            DeviceSelectorRow(selected: $selected)
            captureField
        }
        .padding(.horizontal, 14)
        .padding(.top, 10)
        .animation(.easeOut(duration: 0.2), value: clipboard.transfer)
        // Reserve the keyboard band; the chrome above rides just on top of the
        // keyboard and the touchpad fills the rest. Animate in lock-step with the
        // system keyboard so the split tracks it smoothly.
        .padding(.bottom, keyboard.height)
        .animation(.easeOut(duration: keyboard.animationDuration), value: keyboard.height)
        // We are manually reserving the keyboard band, so opt out of SwiftUI's own
        // keyboard avoidance (it would double-count the inset and push the pad up).
        .ignoresSafeArea(.keyboard, edges: .bottom)
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

/// Bridge SwiftUI's `ScenePhase` onto the SwiftUI-free `ScenePhaseLike` that
/// `AppLifecycle` consumes.
private extension ScenePhase {
    var asLifecyclePhase: ScenePhaseLike {
        switch self {
        case .active: return .active
        case .inactive: return .inactive
        case .background: return .background
        @unknown default: return .inactive
        }
    }
}

#Preview {
    CompanionView()
}
