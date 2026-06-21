import SwiftUI

/// SwiftUI bridge to `TrackpadHostView`. It owns the momentum scroller and
/// translates raw UIKit gesture callbacks into `TrackpadState` mutations + haptic
/// feedback. This is the seam where, later, the same callbacks will additionally
/// emit motion/scroll/click datagrams through mouser-ffi (architecture §9).
struct TrackpadSurface: UIViewRepresentable {
    @ObservedObject var state: TrackpadState

    func makeCoordinator() -> Coordinator { Coordinator(state: state) }

    func makeUIView(context: Context) -> TrackpadHostView {
        let view = TrackpadHostView()
        context.coordinator.attach(to: view)
        return view
    }

    func updateUIView(_ uiView: TrackpadHostView, context: Context) {}

    @MainActor
    final class Coordinator {
        private let state: TrackpadState
        private let momentum = MomentumScroller()
        /// Accumulates scroll travel so we only fire a detent every N points,
        /// like the discrete ratchet of a real trackpad.
        private var scrollAccumulator: CGFloat = 0
        private let detentStride: CGFloat = 24

        init(state: TrackpadState) {
            self.state = state
            momentum.onTick = { [weak self] delta in
                self?.emitScroll(delta, momentum: true)
            }
            momentum.onStop = { [weak self] in
                self?.state.isMomentum = false
            }
        }

        func attach(to view: TrackpadHostView) {
            view.onForceSupportResolved = { [weak self] supported in
                self?.state.forceSupported = supported
            }
            view.onTouchesChanged = { [weak self] points in
                self?.state.activeTouchPoints = points
            }

            view.onMove = { [weak self] delta, _ in
                self?.momentum.stop()
                self?.state.registerMove(delta)
            }

            view.onScroll = { [weak self] delta in
                self?.momentum.stop()
                self?.emitScroll(delta, momentum: false)
            }
            view.onScrollMomentum = { [weak self] velocity in
                guard let self else { return }
                self.state.isMomentum = true
                self.momentum.start(velocity: velocity)
            }

            view.onLeftClick = { [weak self] in
                Haptics.shared.leftClick()
                self?.state.clickCount += 1
                self?.state.report(.leftClick)
            }
            view.onRightClick = { [weak self] in
                Haptics.shared.rightClick()
                self?.state.rightClickCount += 1
                self?.state.report(.rightClick)
            }

            view.onClickDragBegan = { [weak self] in
                Haptics.shared.dragStart()
                self?.momentum.stop()
                self?.state.isClickDragging = true
                self?.state.report(.dragSelect)
            }
            view.onClickDragMoved = { [weak self] delta, _ in
                self?.state.registerMove(delta)
                self?.state.lastEvent = .dragSelect
            }
            view.onClickDragEnded = { [weak self] in
                self?.state.isClickDragging = false
            }

            view.onMagnify = { [weak self] scale in
                self?.state.magnification = scale
                self?.state.lastEvent = .magnify
            }
            view.onRotate = { [weak self] radians in
                self?.state.rotationDegrees = radians * 180 / .pi
                self?.state.lastEvent = .rotate
            }

            view.onForce = { [weak self] force in
                self?.state.force = force
                if force < 0.5 { self?.state.isForceClick = false }
            }
            view.onForceClick = { [weak self] in
                Haptics.shared.forceClick()
                self?.state.isForceClick = true
                self?.state.report(.forceClick)
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
