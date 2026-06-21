import Combine
import Foundation

/// Owns the app's reaction to `scenePhase` transitions (audit R2 — "app lifecycle
/// + reconnect scaffolding").
///
/// When the companion is backgrounded we must stop driving the remote machine:
/// kill any momentum glide, stop streaming motion/scroll/key datagrams, and — once
/// networking exists — yield ownership and send a `Goodbye` so the cluster reclaims
/// the cursor cleanly (architecture §7.1 sleep/wake). When we return to the
/// foreground we trigger a reconnect. Networking isn't wired yet, so the network
/// steps are explicit no-op extension points (`onYieldOwnership` / `onReconnect`)
/// that the FFI layer will fill in; the local-only effects (momentum + streaming
/// gate) are real today.
///
/// `isStreaming` is the single gate every gesture sink checks before it does any
/// work: while backgrounded it is `false`, so no motion/haptics/datagrams are
/// produced even if a stray gesture callback fires.
@MainActor
final class AppLifecycle: ObservableObject {
    /// Whether the app may currently drive the remote (true only while active).
    /// Gesture sinks must no-op when this is false so nothing happens in the
    /// background.
    @Published private(set) var isStreaming = true

    // MARK: - Local effect hooks (wired today)

    /// Stop any in-flight momentum glide. Set by the trackpad layer so the display
    /// link can't keep ticking (and emitting) while backgrounded.
    var stopMomentum: (() -> Void)?

    // MARK: - Network extension points (filled in once mouser-ffi lands)

    /// Begin streaming input to the active machine. Today only flips `isStreaming`;
    /// later (re)starts the motion/scroll/key send paths over the control plane.
    var onStartStreaming: (() -> Void)?
    /// Stop streaming input. Today only flips `isStreaming`; later tears down the
    /// send paths so no datagrams leave while backgrounded.
    var onStopStreaming: (() -> Void)?
    /// Yield ownership and send `Goodbye{Sleep}` to the cluster (architecture
    /// §7.1) so the cursor is reclaimed cleanly when we background. No-op until the
    /// engine/FFI exists.
    var onYieldOwnership: (() -> Void)?
    /// Re-establish the connection and request ownership again on return to the
    /// foreground (architecture §7.2 reconnect). No-op until the engine/FFI exists.
    var onReconnect: (() -> Void)?

    /// React to a `scenePhase` change from the SwiftUI scene.
    func handle(_ phase: ScenePhaseLike) {
        switch phase {
        case .active:
            enterForeground()
        case .inactive:
            // Transient (app switcher, incoming call): stop streaming but don't
            // tear down the session — `.active` will resume, `.background` will
            // fully yield.
            isStreaming = false
            stopMomentum?()
            onStopStreaming?()
        case .background:
            enterBackground()
        }
    }

    private func enterForeground() {
        isStreaming = true
        onStartStreaming?()
        onReconnect?()
    }

    private func enterBackground() {
        // Order matters: stop local work first so nothing is emitted mid-teardown,
        // then yield ownership to the cluster.
        isStreaming = false
        stopMomentum?()
        onStopStreaming?()
        onYieldOwnership?()
    }
}

/// Mirror of SwiftUI's `ScenePhase` so `AppLifecycle` (and its tests) don't need
/// to import SwiftUI. `CompanionView` maps `Environment(\.scenePhase)` onto this.
enum ScenePhaseLike {
    case active
    case inactive
    case background
}
