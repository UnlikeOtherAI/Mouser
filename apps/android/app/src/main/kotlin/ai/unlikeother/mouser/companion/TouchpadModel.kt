package ai.unlikeother.mouser.companion

import androidx.compose.ui.geometry.Offset

/**
 * Discrete events the [GestureEngine] emits as it recognises macOS-trackpad
 * gestures. In the real app these map onto the wire protocol
 * (communication-interface §6: `PointerMotion` deltas, `Scroll {dx,dy,unit}`,
 * pointer button down/up, magnify/rotate). Here they drive local feedback
 * (haptics + the on-screen readout); engine wiring comes later.
 */
sealed interface TouchpadEvent {
    /** Relative cursor motion after the acceleration curve (logical pixels). */
    data class Move(val dx: Float, val dy: Float) : TouchpadEvent

    /** Single tap -> left click (button down+up). */
    data object LeftClick : TouchpadEvent

    /** Two-finger tap -> right (secondary) click. */
    data object RightClick : TouchpadEvent

    /** Tap-then-hold begins a click-drag (primary button held down). */
    data object DragStart : TouchpadEvent

    /** Click-drag released (primary button up). */
    data object DragEnd : TouchpadEvent

    /** Two-finger scroll. `dx,dy` in logical pixels (ScrollUnit.LogicalPixel). */
    data class Scroll(val dx: Float, val dy: Float) : TouchpadEvent

    /** A scroll "detent" was crossed — used purely for haptic feedback. */
    data object ScrollDetent : TouchpadEvent

    /** Pinch magnify. `scale` is the incremental factor for this frame (~1.0). */
    data class Magnify(val scale: Float) : TouchpadEvent

    /** Two-finger rotation, incremental degrees for this frame (+ = clockwise). */
    data class Rotate(val degrees: Float) : TouchpadEvent
}

/** High-level phase, surfaced for the readout label. */
enum class TouchpadPhase { IDLE, MOVING, DRAGGING, SCROLLING, PINCHING }

/**
 * Snapshot of touch state for rendering: the crosshair position(s), the phase,
 * and cumulative/live readouts so the surface is screenshot-verifiable.
 */
data class TouchpadState(
    val phase: TouchpadPhase = TouchpadPhase.IDLE,
    /** Active touch points in surface coordinates (for crosshair rendering). */
    val touches: List<Offset> = emptyList(),
    /** Latest motion delta after accel (logical px). */
    val lastMove: Offset = Offset.Zero,
    /** Latest scroll delta (logical px). */
    val lastScroll: Offset = Offset.Zero,
    /** Cumulative magnification since the current pinch began. */
    val magnification: Float = 1f,
    /** Cumulative rotation degrees since the current pinch began. */
    val rotationDegrees: Float = 0f,
    /** Capacitive "pressure" (MotionEvent.getPressure) of the primary touch. */
    val pressure: Float = 0f,
    /** Human-readable last discrete action, e.g. "LEFT CLICK". */
    val lastAction: String = "—",
    val isTouching: Boolean = false
)
