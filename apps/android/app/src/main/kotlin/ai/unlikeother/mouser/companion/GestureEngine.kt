package ai.unlikeother.mouser.companion

import androidx.compose.ui.geometry.Offset
import kotlin.math.abs
import kotlin.math.atan2
import kotlin.math.hypot
import kotlin.math.sqrt

/** One active pointer as seen by the engine (surface-local px + id). */
data class PointerSample(val id: Long, val position: Offset, val pressure: Float)

/**
 * Recognises the full macOS-trackpad gesture vocabulary from raw multitouch
 * samples and emits [TouchpadEvent]s + an observable [TouchpadState].
 *
 * Recognised gestures (parity with the macOS trackpad, brief "Remote Touchpad"):
 *   - one-finger move      -> relative deltas through an accel curve
 *   - single tap           -> left click
 *   - two-finger tap       -> right click
 *   - two-finger drag      -> scroll with momentum/inertia + detents
 *   - tap-then-hold-drag   -> click-drag (primary button held)
 *   - pinch                -> magnify
 *   - two-finger rotate    -> rotation readout
 *
 * Pure of any Android/Compose plumbing beyond geometry types, so it is unit
 * testable. The owner calls [onPointersChanged] every frame and [tick] on a
 * frame clock to advance inertia; events arrive via the [emit] callback.
 */
class GestureEngine(
    private val config: Config = Config(),
    private val emit: (TouchpadEvent) -> Unit,
    private val onState: (TouchpadState) -> Unit
) {
    data class Config(
        /** Max ms between down and up to count as a tap. */
        val tapTimeoutMs: Long = 250,
        /** Max finger travel (px) before a press stops being a tap. */
        val tapSlopPx: Float = 18f,
        /** ms after a single tap-up within which a press becomes click-drag. */
        val dragArmWindowMs: Long = 320,
        /** Base pointer gain applied to raw finger deltas. */
        val baseGain: Float = 1.0f,
        /** Acceleration: extra gain proportional to finger speed (px/ms). */
        val accelGain: Float = 0.9f,
        /** Cap on the per-axis accelerated gain. */
        val maxGain: Float = 3.2f,
        /** Logical px of scroll travel per haptic detent. */
        val scrollDetentPx: Float = 28f,
        /** Inertia friction per frame (fraction of velocity retained). */
        val inertiaFriction: Float = 0.92f,
        /** Below this speed (px/frame) inertia stops. */
        val inertiaStopPx: Float = 0.35f,
        /** Min distance change (px) before a two-finger gesture is a pinch. */
        val pinchSlopPx: Float = 14f,
        /** Min rotation (deg) before a two-finger gesture counts as rotate. */
        val rotateSlopDeg: Float = 8f
    )

    private enum class Mode { NONE, PENDING, MOVING, DRAGGING, TWO_FINGER, SCROLLING, PINCHING }

    private var mode = Mode.NONE

    // Single-finger move/tap/drag tracking.
    private var primaryId: Long? = null
    private var lastPrimary: Offset = Offset.Zero
    private var pressDownTime: Long = 0
    private var travel: Float = 0f
    private var lastMoveTime: Long = 0
    private var lastTapUpTime: Long = -1
    private var dragArmed = false

    // Two-finger tracking.
    private var twoDownTime: Long = 0
    private var prevCentroid: Offset = Offset.Zero
    private var prevDistance: Float = 0f
    private var prevAngle: Float = 0f
    private var twoTravel: Float = 0f
    private var pinchCommitted = false
    private var scrollAccum: Float = 0f
    private var magnifyAccum: Float = 1f
    private var rotateAccum: Float = 0f

    // Inertia (scroll momentum after lift).
    private var scrollVel = Offset.Zero
    private var inertia = Offset.Zero
    private var inertiaActive = false

    private var lastAction = "—"
    private var pressure = 0f

    /** Feed the current set of active pointers for this frame. */
    fun onPointersChanged(pointers: List<PointerSample>, now: Long) {
        pressure = pointers.firstOrNull()?.pressure ?: 0f
        when (pointers.size) {
            0 -> onAllUp(now)
            1 -> oneFinger(pointers[0], now)
            else -> twoFingers(pointers[0], pointers[1], now)
        }
        publish(pointers)
    }

    /** Advance scroll inertia. Call on each frame while [inertiaActive]. */
    fun tick(now: Long) {
        if (!inertiaActive) return
        inertia = Offset(inertia.x * config.inertiaFriction, inertia.y * config.inertiaFriction)
        val speed = hypot(inertia.x, inertia.y)
        if (speed < config.inertiaStopPx) {
            inertiaActive = false
            inertia = Offset.Zero
            publish(emptyList())
            return
        }
        emitScroll(inertia)
        publish(emptyList())
    }

    val isInertiaActive: Boolean get() = inertiaActive

    // ---- Single finger ----------------------------------------------------

    private fun oneFinger(p: PointerSample, now: Long) {
        val id = primaryId
        if (id == null || id != p.id) {
            beginPrimary(p, now)
            return
        }
        val raw = p.position - lastPrimary
        lastPrimary = p.position
        travel += hypot(raw.x, raw.y)
        val dt = (now - lastMoveTime).coerceAtLeast(1)
        lastMoveTime = now

        when (mode) {
            Mode.PENDING -> {
                if (travel > config.tapSlopPx) {
                    // Pending press became a move/drag.
                    mode = if (dragArmed) {
                        emit(TouchpadEvent.DragStart); lastAction = "DRAG START"; Mode.DRAGGING
                    } else {
                        Mode.MOVING
                    }
                    emitMove(raw, dt)
                }
            }
            Mode.MOVING, Mode.DRAGGING -> emitMove(raw, dt)
            else -> { /* coming back to one finger from two: treat as move */
                mode = Mode.MOVING
                emitMove(raw, dt)
            }
        }
    }

    private fun beginPrimary(p: PointerSample, now: Long) {
        // Cancel any inertia the moment a finger lands (macOS behaviour).
        inertiaActive = false
        inertia = Offset.Zero
        primaryId = p.id
        lastPrimary = p.position
        pressDownTime = now
        lastMoveTime = now
        travel = 0f
        dragArmed = lastTapUpTime >= 0 && (now - lastTapUpTime) <= config.dragArmWindowMs
        mode = Mode.PENDING
    }

    // ---- Two fingers ------------------------------------------------------

    private fun twoFingers(a: PointerSample, b: PointerSample, now: Long) {
        val centroid = Offset((a.position.x + b.position.x) / 2f, (a.position.y + b.position.y) / 2f)
        val distance = hypot(a.position.x - b.position.x, a.position.y - b.position.y)
        val angle = Math.toDegrees(
            atan2((b.position.y - a.position.y).toDouble(), (b.position.x - a.position.x).toDouble())
        ).toFloat()

        if (mode != Mode.TWO_FINGER && mode != Mode.SCROLLING && mode != Mode.PINCHING) {
            // Entering a two-finger gesture.
            primaryId = null
            mode = Mode.TWO_FINGER
            twoDownTime = now
            prevCentroid = centroid
            prevDistance = distance
            prevAngle = angle
            startDistance = distance
            startAngle = angle
            twoTravel = 0f
            pinchCommitted = false
            scrollAccum = 0f
            magnifyAccum = 1f
            rotateAccum = 0f
            scrollVel = Offset.Zero
            return
        }

        val dCentroid = centroid - prevCentroid
        val dDistance = distance - prevDistance
        val dAngle = normalizeDegrees(angle - prevAngle)
        prevCentroid = centroid
        twoTravel += hypot(dCentroid.x, dCentroid.y)

        // Decide pinch vs scroll once movement is meaningful.
        if (mode == Mode.TWO_FINGER) {
            val pinchish = abs(distance - startDistance) > config.pinchSlopPx ||
                abs(normalizeDegrees(angle - startAngle)) > config.rotateSlopDeg
            val scrollish = twoTravel > config.tapSlopPx
            when {
                pinchish && !scrollish -> { mode = Mode.PINCHING; pinchCommitted = true }
                scrollish -> mode = Mode.SCROLLING
            }
        }

        when (mode) {
            Mode.SCROLLING -> {
                emitScroll(dCentroid)
                scrollVel = dCentroid
            }
            Mode.PINCHING -> {
                if (abs(dDistance) > 0.01f && prevDistance > 0f) {
                    val scale = distance / prevDistance
                    magnifyAccum *= scale
                    emit(TouchpadEvent.Magnify(scale))
                }
                if (abs(dAngle) > 0.01f) {
                    rotateAccum += dAngle
                    emit(TouchpadEvent.Rotate(dAngle))
                }
                lastAction = "PINCH ${"%.2f".format(magnifyAccum)}x ${"%+.0f".format(rotateAccum)}°"
            }
            else -> { /* still TWO_FINGER, undecided */ }
        }
        prevDistance = distance
        prevAngle = angle
    }

    // Snapshots captured at gesture start for the pinch/scroll decision.
    private var startDistance = 0f
    private var startAngle = 0f

    // ---- Lift -------------------------------------------------------------

    private fun onAllUp(now: Long) {
        when (mode) {
            Mode.PENDING -> {
                // Press + quick release with little travel = single tap (left click).
                val quick = (now - pressDownTime) <= config.tapTimeoutMs
                if (quick && travel <= config.tapSlopPx) {
                    emit(TouchpadEvent.LeftClick); lastAction = "LEFT CLICK"
                    lastTapUpTime = now
                }
            }
            Mode.DRAGGING -> { emit(TouchpadEvent.DragEnd); lastAction = "DRAG END" }
            Mode.SCROLLING -> startInertia()
            Mode.TWO_FINGER -> {
                // Two fingers down+up quickly with little travel = right click.
                val quick = (now - twoDownTime) <= config.tapTimeoutMs
                if (quick && twoTravel <= config.tapSlopPx) {
                    emit(TouchpadEvent.RightClick); lastAction = "RIGHT CLICK"
                }
            }
            else -> { /* MOVING / PINCHING / NONE: nothing on lift */ }
        }
        primaryId = null
        mode = Mode.NONE
    }

    private fun startInertia() {
        val speed = hypot(scrollVel.x, scrollVel.y)
        if (speed >= config.inertiaStopPx * 3f) {
            inertia = scrollVel
            inertiaActive = true
            lastAction = "SCROLL ↺ inertia"
        }
    }

    // ---- Emission helpers -------------------------------------------------

    private fun emitMove(raw: Offset, dtMs: Long) {
        val gx = gainFor(raw.x, dtMs)
        val gy = gainFor(raw.y, dtMs)
        val dx = raw.x * gx
        val dy = raw.y * gy
        emit(TouchpadEvent.Move(dx, dy))
        lastAction = if (mode == Mode.DRAGGING) "DRAG MOVE" else "MOVE"
        lastMove = Offset(dx, dy)
    }

    /** macOS-like pointer acceleration: gain rises with finger speed, capped. */
    private fun gainFor(componentPx: Float, dtMs: Long): Float {
        val speed = abs(componentPx) / dtMs.toFloat() // px per ms
        val gain = config.baseGain + config.accelGain * sqrt(speed)
        return gain.coerceAtMost(config.maxGain)
    }

    private var lastMove = Offset.Zero
    private var lastScroll = Offset.Zero

    private fun emitScroll(delta: Offset) {
        emit(TouchpadEvent.Scroll(delta.x, delta.y))
        lastScroll = delta
        // Detents on the dominant axis for tactile feedback.
        scrollAccum += abs(delta.y) + abs(delta.x)
        while (scrollAccum >= config.scrollDetentPx) {
            scrollAccum -= config.scrollDetentPx
            emit(TouchpadEvent.ScrollDetent)
        }
        if (mode == Mode.SCROLLING) lastAction = "SCROLL"
    }

    // ---- State publishing -------------------------------------------------

    private fun publish(pointers: List<PointerSample>) {
        val phase = when {
            inertiaActive -> TouchpadPhase.SCROLLING
            mode == Mode.DRAGGING -> TouchpadPhase.DRAGGING
            mode == Mode.SCROLLING -> TouchpadPhase.SCROLLING
            mode == Mode.PINCHING -> TouchpadPhase.PINCHING
            mode == Mode.MOVING || mode == Mode.PENDING -> TouchpadPhase.MOVING
            else -> TouchpadPhase.IDLE
        }
        onState(
            TouchpadState(
                phase = phase,
                touches = pointers.map { it.position },
                lastMove = lastMove,
                lastScroll = lastScroll,
                magnification = magnifyAccum,
                rotationDegrees = rotateAccum,
                pressure = pressure,
                lastAction = lastAction,
                isTouching = pointers.isNotEmpty() || inertiaActive
            )
        )
    }

    private fun normalizeDegrees(d: Float): Float {
        var x = d
        while (x > 180f) x -= 360f
        while (x < -180f) x += 360f
        return x
    }
}
