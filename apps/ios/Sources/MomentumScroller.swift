import QuartzCore
import CoreGraphics

/// Inertial decay for two-finger scrolling (requirement §2: "two-finger drag →
/// scroll WITH momentum/inertia (decay after lift)").
///
/// On lift we keep emitting scroll deltas that decay exponentially from the
/// finger's release velocity, exactly like a macOS trackpad fling. A
/// `CADisplayLink` drives the decay so it is frame-locked and smooth. Each tick
/// reports the decayed delta to a callback so the readout (and, later, the real
/// scroll datagrams) stay live during the glide.
@MainActor
final class MomentumScroller {
    /// Per-frame multiplier applied to velocity. ~0.95 @ 60fps ≈ a ~0.6s glide,
    /// close to the macOS feel.
    var friction: CGFloat = 0.94
    /// Below this speed (points/sec) the glide is considered finished.
    var minimumSpeed: CGFloat = 6

    /// Called every frame with the decayed per-frame scroll delta.
    var onTick: ((CGSize) -> Void)?
    /// Called once when the glide finishes (velocity died or cancelled).
    var onStop: (() -> Void)?

    private var velocity: CGSize = .zero
    private var displayLink: CADisplayLink?
    private var lastTimestamp: CFTimeInterval = 0

    var isDecaying: Bool { displayLink != nil }

    /// Begin a momentum glide from a release velocity (points/sec).
    func start(velocity: CGSize) {
        stop()
        let speed = (velocity.width * velocity.width + velocity.height * velocity.height).squareRoot()
        guard speed > minimumSpeed else { return }
        self.velocity = velocity
        lastTimestamp = 0
        let link = CADisplayLink(target: self, selector: #selector(step(_:)))
        link.add(to: .main, forMode: .common)
        displayLink = link
    }

    /// Cancel any in-flight glide (e.g. a new touch landed).
    func stop() {
        displayLink?.invalidate()
        displayLink = nil
        if velocity != .zero {
            velocity = .zero
            onStop?()
        }
    }

    @objc private func step(_ link: CADisplayLink) {
        if lastTimestamp == 0 { lastTimestamp = link.timestamp }
        let dt = CGFloat(link.timestamp - lastTimestamp)
        lastTimestamp = link.timestamp

        // Per-frame delta from current velocity.
        let delta = CGSize(width: velocity.width * dt, height: velocity.height * dt)
        onTick?(delta)

        // Decay velocity. Normalise friction to the actual frame duration so the
        // glide length is consistent regardless of 60/120 Hz.
        let decay = pow(friction, dt * 60)
        velocity = CGSize(width: velocity.width * decay, height: velocity.height * decay)

        let speed = (velocity.width * velocity.width + velocity.height * velocity.height).squareRoot()
        if speed < minimumSpeed {
            stop()
        }
    }
}
