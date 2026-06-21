import CoreGraphics

/// Pointer-acceleration curve, mimicking macOS trackpad behaviour (requirement
/// §2: "relative deltas, with a pointer-acceleration curve like macOS").
///
/// macOS does not map finger movement 1:1 to cursor movement. Slow movements are
/// damped (for precision) and fast movements are amplified (to cross the screen
/// without lifting). We approximate this with a speed-dependent gain applied to
/// each raw delta. The raw delta is in points-per-event; we scale gain between a
/// minimum (precise) and maximum (fast flick) based on the finger speed.
enum PointerAcceleration {
    /// Gain applied to a tiny, slow movement (sub-pixel precision regime).
    static let minGain: CGFloat = 0.6
    /// Gain applied to a fast flick (ballistic regime).
    static let maxGain: CGFloat = 2.6
    /// Finger speed (points/sec) at which gain saturates to `maxGain`.
    static let saturationSpeed: CGFloat = 1600

    /// Map a raw per-event delta + the instantaneous speed to an accelerated
    /// delta, the value that will later become a relative motion datagram.
    static func accelerate(delta: CGSize, speed: CGFloat) -> CGSize {
        let gain = gain(forSpeed: speed)
        return CGSize(width: delta.width * gain, height: delta.height * gain)
    }

    /// The acceleration curve itself: a smooth ease from `minGain` to `maxGain`.
    static func gain(forSpeed speed: CGFloat) -> CGFloat {
        let t = min(max(speed / saturationSpeed, 0), 1)
        // Smoothstep for a gentle, macOS-like ramp rather than a hard linear ramp.
        let eased = t * t * (3 - 2 * t)
        return minGain + (maxGain - minGain) * eased
    }
}
