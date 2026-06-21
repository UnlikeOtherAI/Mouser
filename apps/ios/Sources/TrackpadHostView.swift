import UIKit

/// The UIKit trackpad surface that replicates the macOS trackpad (requirement
/// §2). SwiftUI's gesture system cannot reliably distinguish finger counts or
/// expose `UITouch.force`, so the full gesture set lives here on a `UIView` with
/// native `UIGestureRecognizer`s, and the recognised events are pushed up to
/// `TrackpadState` via the bound closures.
///
/// Gesture map (mirrors macOS):
///   • one-finger pan            → cursor move (accelerated relative deltas)
///   • single tap                → left click
///   • two-finger tap            → right / secondary click
///   • two-finger pan            → scroll, with momentum after lift
///   • tap-then-hold-then-pan    → click-and-drag selection
///   • two-finger pinch          → magnify (advanced)
///   • two-finger rotation       → rotate (advanced)
///   • deep press (UITouch.force)→ force-click, where Force Touch is available
final class TrackpadHostView: UIView {
    // Event sinks (wired by the representable to TrackpadState mutations).
    var onMove: ((CGSize, CGFloat) -> Void)?          // accelerated delta, speed
    var onScroll: ((CGSize) -> Void)?                 // live scroll delta
    var onScrollMomentum: ((CGSize) -> Void)?         // release velocity (pts/s)
    var onLeftClick: (() -> Void)?
    var onRightClick: (() -> Void)?
    var onClickDragBegan: (() -> Void)?
    var onClickDragMoved: ((CGSize, CGFloat) -> Void)?
    var onClickDragEnded: (() -> Void)?
    var onMagnify: ((CGFloat) -> Void)?               // absolute scale
    var onRotate: ((CGFloat) -> Void)?                // radians
    var onForce: ((CGFloat) -> Void)?                 // 0…1 normalised
    var onForceClick: (() -> Void)?
    var onTouchesChanged: (([CGPoint]) -> Void)?      // for live dots
    var onForceSupportResolved: ((Bool) -> Void)?

    private var lastPanLocation: CGPoint?
    private var hasResolvedForceSupport = false
    private var forceClickLatched = false

    override init(frame: CGRect) {
        super.init(frame: frame)
        isMultipleTouchEnabled = true
        backgroundColor = .clear
        installRecognizers()
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) not supported") }

    // MARK: - Recognizer wiring

    private func installRecognizers() {
        // One-finger move.
        let onePan = UIPanGestureRecognizer(target: self, action: #selector(handleOnePan(_:)))
        onePan.minimumNumberOfTouches = 1
        onePan.maximumNumberOfTouches = 1
        onePan.delegate = self

        // Two-finger scroll.
        let twoPan = UIPanGestureRecognizer(target: self, action: #selector(handleTwoPan(_:)))
        twoPan.minimumNumberOfTouches = 2
        twoPan.maximumNumberOfTouches = 2
        twoPan.delegate = self

        // Single (left) click.
        let singleTap = UITapGestureRecognizer(target: self, action: #selector(handleSingleTap(_:)))
        singleTap.numberOfTapsRequired = 1
        singleTap.numberOfTouchesRequired = 1
        singleTap.delegate = self

        // Two-finger (right) click.
        let twoTap = UITapGestureRecognizer(target: self, action: #selector(handleTwoTap(_:)))
        twoTap.numberOfTapsRequired = 1
        twoTap.numberOfTouchesRequired = 2
        twoTap.delegate = self

        // Tap-then-hold-drag → click-and-drag selection. A long-press that, once
        // engaged, tracks movement (UILongPressGestureRecognizer reports
        // .changed with the moving location).
        let clickDrag = UILongPressGestureRecognizer(target: self, action: #selector(handleClickDrag(_:)))
        clickDrag.numberOfTapsRequired = 1   // tap first, THEN press-and-hold
        clickDrag.minimumPressDuration = 0.12
        clickDrag.allowableMovement = .greatestFiniteMagnitude
        clickDrag.delegate = self

        // Pinch (magnify) + rotation (advanced).
        let pinch = UIPinchGestureRecognizer(target: self, action: #selector(handlePinch(_:)))
        pinch.delegate = self
        let rotate = UIRotationGestureRecognizer(target: self, action: #selector(handleRotate(_:)))
        rotate.delegate = self

        // A single tap should not also begin a click-drag; let click-drag win
        // before a plain single tap fires.
        singleTap.require(toFail: clickDrag)

        // One-finger pan and click-drag both move the cursor. Without this, a
        // tap-then-hold-then-drag drives BOTH recognizers' .changed in parallel
        // and the cursor delta is doubled during a drag-select (audit R2). Make
        // the plain pan defer to click-drag so exactly one path drives motion:
        // once the long-press engages, only `handleClickDrag` reports movement.
        onePan.require(toFail: clickDrag)

        [onePan, twoPan, singleTap, twoTap, clickDrag, pinch, rotate].forEach(addGestureRecognizer)
    }

    // MARK: - One-finger move

    @objc private func handleOnePan(_ gr: UIPanGestureRecognizer) {
        switch gr.state {
        case .began:
            lastPanLocation = gr.location(in: self)
        case .changed:
            let location = gr.location(in: self)
            let raw = delta(from: lastPanLocation, to: location)
            lastPanLocation = location
            let velocity = gr.velocity(in: self)
            let speed = magnitude(velocity)
            let accelerated = PointerAcceleration.accelerate(delta: raw, speed: speed)
            onMove?(accelerated, speed)
        case .ended, .cancelled, .failed:
            lastPanLocation = nil
        default:
            break
        }
    }

    // MARK: - Two-finger scroll + momentum

    @objc private func handleTwoPan(_ gr: UIPanGestureRecognizer) {
        switch gr.state {
        case .began:
            lastPanLocation = gr.location(in: self)
        case .changed:
            let location = gr.location(in: self)
            let raw = delta(from: lastPanLocation, to: location)
            lastPanLocation = location
            onScroll?(raw)
        case .ended:
            // Hand the release velocity to the momentum scroller for inertia.
            let v = gr.velocity(in: self)
            onScrollMomentum?(CGSize(width: v.x, height: v.y))
            lastPanLocation = nil
        case .cancelled, .failed:
            lastPanLocation = nil
        default:
            break
        }
    }

    // MARK: - Clicks

    @objc private func handleSingleTap(_ gr: UITapGestureRecognizer) {
        guard gr.state == .ended else { return }
        onLeftClick?()
    }

    @objc private func handleTwoTap(_ gr: UITapGestureRecognizer) {
        guard gr.state == .ended else { return }
        onRightClick?()
    }

    // MARK: - Click-and-drag (selection)

    @objc private func handleClickDrag(_ gr: UILongPressGestureRecognizer) {
        let location = gr.location(in: self)
        switch gr.state {
        case .began:
            lastPanLocation = location
            onClickDragBegan?()
        case .changed:
            let raw = delta(from: lastPanLocation, to: location)
            lastPanLocation = location
            let velocity = magnitude(raw) * 60   // approx pts/s for the curve
            let accelerated = PointerAcceleration.accelerate(delta: raw, speed: velocity)
            onClickDragMoved?(accelerated, velocity)
        case .ended, .cancelled, .failed:
            lastPanLocation = nil
            onClickDragEnded?()
        default:
            break
        }
    }

    // MARK: - Pinch + rotate (advanced)

    @objc private func handlePinch(_ gr: UIPinchGestureRecognizer) {
        guard gr.state == .began || gr.state == .changed else { return }
        onMagnify?(gr.scale)
    }

    @objc private func handleRotate(_ gr: UIRotationGestureRecognizer) {
        guard gr.state == .began || gr.state == .changed else { return }
        onRotate?(gr.rotation)
    }

    // MARK: - Touch tracking + Force Touch (requirement §4)

    override func touchesBegan(_ touches: Set<UITouch>, with event: UIEvent?) {
        super.touchesBegan(touches, with: event)
        resolveForceSupportIfNeeded()
        forceClickLatched = false
        publishTouches(event)
        trackForce(touches)
    }

    override func touchesMoved(_ touches: Set<UITouch>, with event: UIEvent?) {
        super.touchesMoved(touches, with: event)
        publishTouches(event)
        trackForce(touches)
    }

    override func touchesEnded(_ touches: Set<UITouch>, with event: UIEvent?) {
        super.touchesEnded(touches, with: event)
        publishTouches(event)
        if (event?.allTouches?.filter { $0.phase != .ended && $0.phase != .cancelled }.isEmpty) ?? true {
            forceClickLatched = false
            onForce?(0)
        }
    }

    override func touchesCancelled(_ touches: Set<UITouch>, with event: UIEvent?) {
        super.touchesCancelled(touches, with: event)
        publishTouches(event)
        forceClickLatched = false
        onForce?(0)
    }

    private func resolveForceSupportIfNeeded() {
        guard !hasResolvedForceSupport else { return }
        hasResolvedForceSupport = true
        let available = traitCollection.forceTouchCapability == .available
        onForceSupportResolved?(available)
    }

    private func trackForce(_ touches: Set<UITouch>) {
        // Gracefully skip on devices without Force Touch (iPhone XR+, simulator):
        // capability is .unavailable / .unknown there, so we never read force and
        // never show the force UI (requirement §4).
        guard traitCollection.forceTouchCapability == .available,
              let touch = touches.first,
              touch.maximumPossibleForce > 0 else { return }
        let normalised = touch.force / touch.maximumPossibleForce
        onForce?(normalised)
        if normalised >= 0.75, !forceClickLatched {
            forceClickLatched = true
            onForceClick?()
        } else if normalised < 0.5 {
            forceClickLatched = false
        }
    }

    private func publishTouches(_ event: UIEvent?) {
        let points = (event?.allTouches ?? [])
            .filter { $0.phase != .ended && $0.phase != .cancelled }
            .map { $0.location(in: self) }
        onTouchesChanged?(points)
    }

    // MARK: - Geometry helpers

    private func delta(from: CGPoint?, to: CGPoint) -> CGSize {
        guard let from else { return .zero }
        return CGSize(width: to.x - from.x, height: to.y - from.y)
    }

    private func magnitude(_ p: CGPoint) -> CGFloat {
        (p.x * p.x + p.y * p.y).squareRoot()
    }

    private func magnitude(_ s: CGSize) -> CGFloat {
        (s.width * s.width + s.height * s.height).squareRoot()
    }
}

// MARK: - Simultaneous recognition

extension TrackpadHostView: UIGestureRecognizerDelegate {
    /// Let pinch + rotate + two-finger pan all run together (macOS recognises
    /// magnify/rotate/scroll simultaneously), and let taps coexist with pans.
    func gestureRecognizer(
        _ gestureRecognizer: UIGestureRecognizer,
        shouldRecognizeSimultaneouslyWith other: UIGestureRecognizer
    ) -> Bool {
        true
    }
}
