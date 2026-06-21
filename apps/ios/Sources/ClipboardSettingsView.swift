import SwiftUI

/// Clipboard settings view (audit R2 — clipboard UI hooks), mirroring the
/// Clipboard section in `mouser-clipboard::ClipboardSettings` (§7.7): master
/// on/off, per-format gates (text / images / files), max auto-sync size,
/// prefer-native-Apple, and sync direction.
///
/// Fed by a local `ClipboardModel` with mock state; no networking. Once the FFI
/// lands these controls write to `ClipboardEngine::set_settings`.
struct ClipboardSettingsView: View {
    @ObservedObject var model: ClipboardModel

    /// Max auto-sync limit edited in MB (0 = unlimited). Mirrors
    /// `max_auto_sync_bytes`, converted on write.
    @State private var maxSizeMB: Double = 0

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 16) {
                masterSection
                if model.settings.sharedClipboard {
                    formatsSection
                    sizeSection
                    behaviourSection
                }
            }
            .padding(18)
        }
        .background(Color.black.ignoresSafeArea())
        .preferredColorScheme(.dark)
        .navigationTitle("Clipboard")
        .onAppear { maxSizeMB = Double(model.settings.maxAutoSyncBytes) / 1_048_576 }
        .onChange(of: maxSizeMB) { _, mb in
            model.settings.maxAutoSyncBytes = UInt64((mb * 1_048_576).rounded())
        }
        .accessibilityIdentifier("clipboard.settings")
    }

    // MARK: - Master

    private var masterSection: some View {
        card {
            Toggle(isOn: $model.settings.sharedClipboard) {
                label("Shared clipboard", "Sync copy & paste across your devices",
                      systemImage: "doc.on.clipboard")
            }
            .tint(.green)
            .accessibilityIdentifier("clipboard.master")
        }
    }

    // MARK: - Per-format gates

    private var formatsSection: some View {
        section("FORMATS") {
            toggleRow("Text", subtitle: "Plain text, HTML, RTF",
                      systemImage: "textformat",
                      isOn: $model.settings.syncText,
                      id: "clipboard.text")
            divider
            toggleRow("Images", subtitle: "PNG images",
                      systemImage: "photo",
                      isOn: $model.settings.syncImages,
                      id: "clipboard.images")
            divider
            toggleRow("Files", subtitle: "File references (uri list)",
                      systemImage: "doc",
                      isOn: $model.settings.syncFiles,
                      id: "clipboard.files")
        }
    }

    // MARK: - Max auto-sync size

    private var sizeSection: some View {
        section("AUTO-SYNC LIMIT") {
            VStack(alignment: .leading, spacing: 10) {
                HStack {
                    label("Max size", "Skip auto-pull above this", systemImage: "arrow.down.circle")
                    Spacer()
                    Text(maxSizeMB <= 0 ? "Unlimited" : String(format: "%.0f MB", maxSizeMB))
                        .font(.system(.footnote, design: .monospaced).weight(.semibold))
                        .foregroundStyle(.green)
                }
                Slider(value: $maxSizeMB, in: 0...100, step: 5)
                    .tint(.green)
                    .accessibilityIdentifier("clipboard.maxSize")
            }
        }
    }

    // MARK: - Behaviour (prefer-native + direction)

    private var behaviourSection: some View {
        section("BEHAVIOUR") {
            Toggle(isOn: $model.settings.preferNativeApple) {
                label("Prefer Apple clipboard",
                      "Let Handoff sync between Apple devices",
                      systemImage: "apple.logo")
            }
            .tint(.green)
            .accessibilityIdentifier("clipboard.preferNative")

            divider

            VStack(alignment: .leading, spacing: 8) {
                label("Direction", "Which way clipboard content flows",
                      systemImage: "arrow.left.arrow.right")
                Picker("Direction", selection: $model.settings.direction) {
                    ForEach(SyncDirection.allCases) { dir in
                        Text(dir.rawValue).tag(dir)
                    }
                }
                .pickerStyle(.segmented)
                .accessibilityIdentifier("clipboard.direction")
            }
        }
    }

    // MARK: - Building blocks

    private func toggleRow(_ title: String, subtitle: String, systemImage: String,
                           isOn: Binding<Bool>, id: String) -> some View {
        Toggle(isOn: isOn) {
            label(title, subtitle, systemImage: systemImage)
        }
        .tint(.green)
        .accessibilityIdentifier(id)
    }

    private func label(_ title: String, _ subtitle: String, systemImage: String) -> some View {
        HStack(spacing: 12) {
            Image(systemName: systemImage)
                .font(.system(size: 16))
                .foregroundStyle(.white.opacity(0.7))
                .frame(width: 24)
            VStack(alignment: .leading, spacing: 2) {
                Text(title)
                    .font(.subheadline.weight(.semibold))
                    .foregroundStyle(.white)
                Text(subtitle)
                    .font(.caption)
                    .foregroundStyle(.white.opacity(0.45))
            }
        }
    }

    private var divider: some View {
        Rectangle()
            .fill(Color.white.opacity(0.07))
            .frame(height: 1)
    }

    private func section<Content: View>(_ title: String,
                                        @ViewBuilder _ content: () -> Content) -> some View {
        VStack(alignment: .leading, spacing: 10) {
            Text(title)
                .font(.caption2.weight(.bold))
                .tracking(1.6)
                .foregroundStyle(.white.opacity(0.4))
                .padding(.leading, 4)
            card { content() }
        }
    }

    private func card<Content: View>(@ViewBuilder _ content: () -> Content) -> some View {
        VStack(alignment: .leading, spacing: 12) {
            content()
        }
        .padding(14)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(
            RoundedRectangle(cornerRadius: 14, style: .continuous)
                .fill(Color(white: 0.12))
                .overlay(
                    RoundedRectangle(cornerRadius: 14, style: .continuous)
                        .strokeBorder(Color.white.opacity(0.08), lineWidth: 1)
                )
        )
    }
}

#Preview {
    NavigationStack {
        ClipboardSettingsView(model: ClipboardModel())
    }
}
