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
    @State private var captured: String = ""
    /// The text we have already forwarded to the peer. We diff every `captured`
    /// change against this to derive the keystrokes to send (appended chars →
    /// typed keys; removed chars → Backspace). Kept in lock-step with `captured`
    /// so the field stays usable and what's on screen always reflects what was
    /// sent. See `forwardKeystrokes(from:to:)`.
    @State private var lastForwarded: String = ""
    @FocusState private var keyboardFocused: Bool
    @Environment(\.verticalSizeClass) private var verticalSizeClass
    @Environment(\.scenePhase) private var scenePhase

    @StateObject private var peers = PeerStore()
    @StateObject private var browser = PeerBrowser()
    @StateObject private var keyboard = KeyboardObserver()
    @StateObject private var lifecycle = AppLifecycle()
    @StateObject private var clipboard = ClipboardModel()
    @StateObject private var mouser = MouserClient()
    @State private var showClipboardSettings = false

    /// Landscape on iPhone collapses the height into `.compact`; that is our
    /// signal to go full-screen-trackpad.
    private var isLandscape: Bool { verticalSizeClass == .compact }

    /// Name shown in the "Controlling:" chrome — only when the engine bridge
    /// reports a live connection, so the banner reflects `mouser.isConnected`
    /// rather than mere selection.
    private var controllingName: String? {
        mouser.isConnected ? peers.selected?.name : nil
    }

    /// Dial a tapped peer (host/port/device_id resolved by `PeerBrowser`).
    private func connect(to peer: Peer) {
        mouser.connect(host: peer.host, port: peer.port, peerId: peer.deviceId)
    }

    /// Forward the difference between what we last sent (`lastForwarded`) and the
    /// new field value as HID key events. We find the common prefix, send a
    /// Backspace for every removed character, then type every newly-added
    /// character. This one rule covers plain typing (append), deletion
    /// (Backspace), and autocorrect/paste replacements (Backspaces + retype).
    ///
    /// We diff against `lastForwarded` (our state) rather than the value SwiftUI
    /// hands `onChange`, so a programmatic clear that pre-sets `lastForwarded`
    /// (e.g. on submit) produces no phantom keystrokes. `lastForwarded` is always
    /// advanced to `new` so the on-screen text and the sent stream stay in sync.
    private func forwardKeystrokes(to new: String) {
        let old = lastForwarded
        defer { lastForwarded = new }
        guard mouser.isConnected else { return }

        let common = old.commonPrefix(with: new)
        let removed = old.count - common.count
        for _ in 0..<removed { mouser.tapKey(HidKeymap.backspaceUsage) }
        // `common` is a fresh String, so index `new` by the shared prefix length.
        let addedStart = new.index(new.startIndex, offsetBy: common.count)
        for character in new[addedStart...] { mouser.type(character) }
    }

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
        .onAppear {
            keyboardFocused = !isLandscape
            // Start Bonjour/mDNS discovery of computers running mouserd on the LAN.
            browser.start()
        }
        .onDisappear { browser.stop() }
        // Funnel resolved mDNS peers into the selector store (the single seam where
        // real discovery results land).
        .onChange(of: browser.peers) { _, discovered in
            peers.replace(with: discovered)
        }
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
            // Yield ownership + close the connection when backgrounded (a graceful
            // Goodbye), matching the Android lifecycle.
            if phase == .background {
                mouser.disconnect()
            }
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
            TouchpadView(deviceName: controllingName, compact: true, client: mouser)
                .frame(maxWidth: .infinity, maxHeight: .infinity)

            clipboardChrome
            HStack(spacing: 10) {
                ControllingBanner(deviceName: controllingName)
                clipboardButton
            }
            DeviceSelectorRow(store: peers, onSelect: connect)
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
        TouchpadView(deviceName: controllingName, compact: false, client: mouser)
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
                // Forward each field edit as keystrokes (diffed against what we
                // last sent). Disabling autocorrect/autocapitalisation above keeps
                // the diff a clean append/delete rather than surprise rewrites.
                .onChange(of: captured) { _, new in
                    forwardKeystrokes(to: new)
                }
                // Send on the keyboard's Send (Return) key, then clear the field
                // for the next line. Reset `lastForwarded` to empty BEFORE clearing
                // `captured` so the resulting `onChange` diffs ""→"" — no phantom
                // Backspaces for the text we just sent as Return-terminated input.
                .onSubmit {
                    mouser.tapKey(HidKeymap.returnUsage)
                    lastForwarded = ""
                    captured = ""
                }
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
