import SwiftUI

/// SwiftUI bridge to `TrackpadHostView`. It owns the momentum scroller and
/// translates raw UIKit gesture callbacks into `TrackpadState` mutations + haptic
/// feedback. This is the seam where, later, the same callbacks will additionally
/// emit motion/scroll/click datagrams through mouser-ffi (architecture §9).
struct TrackpadSurface: UIViewRepresentable {
    @ObservedObject var state: TrackpadState
    /// Drives the streaming gate and lets the lifecycle stop momentum on
    /// background (audit R2). Passed down from `CompanionView`.
    let lifecycle: AppLifecycle

    func makeCoordinator() -> Coordinator { Coordinator(state: state, lifecycle: lifecycle) }

    func makeUIView(context: Context) -> TrackpadHostView {
        let view = TrackpadHostView()
        context.coordinator.attach(to: view)
        return view
    }

    func updateUIView(_ uiView: TrackpadHostView, context: Context) {}

    @MainActor
    final class Coordinator {
        private let state: TrackpadState
        private let lifecycle: AppLifecycle
        private let momentum = MomentumScroller()
        /// Accumulates scroll travel so we only fire a detent every N points,
        /// like the discrete ratchet of a real trackpad.
        private var scrollAccumulator: CGFloat = 0
        private let detentStride: CGFloat = 24

        init(state: TrackpadState, lifecycle: AppLifecycle) {
            self.state = state
            self.lifecycle = lifecycle
            momentum.onTick = { [weak self] delta in
                self?.emitScroll(delta, momentum: true)
            }
            momentum.onStop = { [weak self] in
                self?.state.isMomentum = false
            }
            // Let the lifecycle kill an in-flight glide when the app backgrounds
            // so the CADisplayLink can't keep emitting while inactive.
            lifecycle.stopMomentum = { [weak self] in self?.momentum.stop() }
        }

        /// Whether gesture callbacks may do work right now. While the app is
        /// backgrounded/inactive `isStreaming` is false, so every sink no-ops and
        /// nothing is emitted (audit R2 — "no work happens while backgrounded").
        private var canStream: Bool { lifecycle.isStreaming }

        func attach(to view: TrackpadHostView) {
            view.onForceSupportResolved = { [weak self] supported in
                self?.state.forceSupported = supported
            }
            view.onTouchesChanged = { [weak self] points in
                self?.state.activeTouchPoints = points
            }

            view.onMove = { [weak self] delta, _ in
                guard let self, self.canStream else { return }
                self.momentum.stop()
                self.state.registerMove(delta)
            }

            view.onScroll = { [weak self] delta in
                guard let self, self.canStream else { return }
                self.momentum.stop()
                self.emitScroll(delta, momentum: false)
            }
            view.onScrollMomentum = { [weak self] velocity in
                guard let self, self.canStream else { return }
                self.state.isMomentum = true
                self.momentum.start(velocity: velocity)
            }

            view.onLeftClick = { [weak self] in
                guard let self, self.canStream else { return }
                Haptics.shared.leftClick()
                self.state.clickCount += 1
                self.state.report(.leftClick)
            }
            view.onRightClick = { [weak self] in
                guard let self, self.canStream else { return }
                Haptics.shared.rightClick()
                self.state.rightClickCount += 1
                self.state.report(.rightClick)
            }

            view.onClickDragBegan = { [weak self] in
                guard let self, self.canStream else { return }
                Haptics.shared.dragStart()
                self.momentum.stop()
                self.state.isClickDragging = true
                self.state.report(.dragSelect)
            }
            view.onClickDragMoved = { [weak self] delta, _ in
                guard let self, self.canStream else { return }
                self.state.registerMove(delta)
                self.state.lastEvent = .dragSelect
            }
            view.onClickDragEnded = { [weak self] in
                self?.state.isClickDragging = false
            }

            view.onMagnify = { [weak self] scale in
                guard let self, self.canStream else { return }
                self.state.magnification = scale
                self.state.lastEvent = .magnify
            }
            view.onRotate = { [weak self] radians in
                guard let self, self.canStream else { return }
                self.state.rotationDegrees = radians * 180 / .pi
                self.state.lastEvent = .rotate
            }

            view.onForce = { [weak self] force in
                guard let self, self.canStream else { return }
                self.state.force = force
                if force < 0.5 { self.state.isForceClick = false }
            }
            view.onForceClick = { [weak self] in
                guard let self, self.canStream else { return }
                Haptics.shared.forceClick()
                self.state.isForceClick = true
                self.state.report(.forceClick)
            }
        }

        /// Apply a scroll delta to state and fire a detent haptic every stride.
        private func emitScroll(_ delta: CGSize, momentum: Bool) {
            state.registerScroll(delta, momentum: momentum)
            scrollAccumulator += abs(delta.height) + abs(delta.width)
            while scrollAccumulator >= detentStride {
                scrollAccumulator -= detentStride
                Haptics.shared.scrollDetent()
                state.scrollDetentCount += 1
            }
        }
    }
}
