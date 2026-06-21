package ai.unlikeother.mouser.companion

import androidx.compose.ui.geometry.Offset
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * Verifies the macOS-trackpad gesture recognition independently of Android.
 * The engine is pure (geometry in, [TouchpadEvent]s out), so it is fully
 * unit-testable on the JVM.
 */
class GestureEngineTest {

    private val events = mutableListOf<TouchpadEvent>()
    private fun engine() = GestureEngine(emit = { events.add(it) }, onState = {})

    private fun p(id: Long, x: Float, y: Float) = PointerSample(id, Offset(x, y), 1f)

    @Test
    fun singleTap_emitsLeftClick() {
        val e = engine()
        e.onPointersChanged(listOf(p(1, 100f, 100f)), now = 0)
        e.onPointersChanged(listOf(p(1, 101f, 100f)), now = 30) // within slop
        e.onPointersChanged(emptyList(), now = 60)              // quick up
        assertTrue(events.any { it is TouchpadEvent.LeftClick })
        assertTrue(events.none { it is TouchpadEvent.Move })
    }

    @Test
    fun longPressNoMove_isNotAClick() {
        val e = engine()
        e.onPointersChanged(listOf(p(1, 100f, 100f)), now = 0)
        e.onPointersChanged(emptyList(), now = 1000) // held too long
        assertTrue(events.none { it is TouchpadEvent.LeftClick })
    }

    @Test
    fun oneFingerDrag_emitsMoveWithAccel() {
        val e = engine()
        e.onPointersChanged(listOf(p(1, 100f, 100f)), now = 0)
        e.onPointersChanged(listOf(p(1, 140f, 100f)), now = 16) // fast 40px in 16ms
        val move = events.filterIsInstance<TouchpadEvent.Move>().first()
        // Acceleration must amplify a fast flick beyond the raw delta.
        assertTrue("expected accelerated dx>40 got ${move.dx}", move.dx > 40f)
    }

    @Test
    fun twoFingerTap_emitsRightClick() {
        val e = engine()
        e.onPointersChanged(listOf(p(1, 100f, 100f), p(2, 160f, 100f)), now = 0)
        e.onPointersChanged(emptyList(), now = 80) // quick two-finger up
        assertTrue(events.any { it is TouchpadEvent.RightClick })
    }

    @Test
    fun twoFingerDrag_emitsScrollAndDetents() {
        val e = engine()
        e.onPointersChanged(listOf(p(1, 100f, 200f), p(2, 160f, 200f)), now = 0)
        // Move both fingers up together (scroll) far enough for detents.
        var y = 200f
        var t = 0L
        repeat(8) {
            y -= 12f; t += 16
            e.onPointersChanged(listOf(p(1, 100f, y), p(2, 160f, y)), now = t)
        }
        assertTrue(events.any { it is TouchpadEvent.Scroll })
        assertTrue(events.any { it is TouchpadEvent.ScrollDetent })
    }

    @Test
    fun tapThenHoldDrag_emitsDragStartMoveEnd() {
        val e = engine()
        // First a quick tap.
        e.onPointersChanged(listOf(p(1, 100f, 100f)), now = 0)
        e.onPointersChanged(emptyList(), now = 40)
        // Then press again within the arm window and move -> click-drag.
        e.onPointersChanged(listOf(p(2, 100f, 100f)), now = 120)
        e.onPointersChanged(listOf(p(2, 140f, 140f)), now = 140)
        e.onPointersChanged(listOf(p(2, 180f, 180f)), now = 160)
        e.onPointersChanged(emptyList(), now = 200)
        val order = events.map { it::class.simpleName }
        assertTrue("DragStart missing in $order", events.any { it is TouchpadEvent.DragStart })
        assertTrue(events.any { it is TouchpadEvent.Move })
        assertTrue("DragEnd missing in $order", events.any { it is TouchpadEvent.DragEnd })
        assertEquals(1, events.count { it is TouchpadEvent.LeftClick }) // only the first tap
    }

    @Test
    fun pinchOut_emitsMagnifyGreaterThanOne() {
        val e = engine()
        e.onPointersChanged(listOf(p(1, 150f, 200f), p(2, 200f, 200f)), now = 0)
        // Spread the fingers apart.
        var sep = 50f
        var t = 0L
        repeat(6) {
            sep += 20f; t += 16
            e.onPointersChanged(
                listOf(p(1, 175f - sep / 2, 200f), p(2, 175f + sep / 2, 200f)),
                now = t
            )
        }
        val mags = events.filterIsInstance<TouchpadEvent.Magnify>()
        assertTrue("expected magnify events", mags.isNotEmpty())
        assertTrue("expected zoom-in scale>1", mags.any { it.scale > 1f })
    }

    @Test
    fun twoFingerRotate_emitsRotation() {
        val e = engine()
        // Two fingers, then rotate the pair about their centroid.
        e.onPointersChanged(listOf(p(1, 100f, 200f), p(2, 200f, 200f)), now = 0)
        val cx = 150f; val cy = 200f; val r = 50f
        var t = 0L
        for (i in 1..8) {
            val a = Math.toRadians((i * 6).toDouble())
            val ax = (cx - r * Math.cos(a)).toFloat()
            val ay = (cy - r * Math.sin(a)).toFloat()
            val bx = (cx + r * Math.cos(a)).toFloat()
            val by = (cy + r * Math.sin(a)).toFloat()
            t += 16
            e.onPointersChanged(listOf(p(1, ax, ay), p(2, bx, by)), now = t)
        }
        assertTrue(events.any { it is TouchpadEvent.Rotate })
    }
}
