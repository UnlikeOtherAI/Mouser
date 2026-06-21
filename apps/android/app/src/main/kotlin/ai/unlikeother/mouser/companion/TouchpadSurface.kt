package ai.unlikeother.mouser.companion

import androidx.compose.foundation.Canvas
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.runtime.withFrameMillis
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.graphics.Brush
import androidx.compose.ui.input.pointer.PointerEventPass
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.unit.dp

/**
 * The remote touchpad surface (brief: "Touchpad above").
 *
 * Captures raw multitouch via [pointerInput] and feeds it to a [GestureEngine]
 * that recognises the full macOS-trackpad vocabulary. Renders the same dark
 * gradient surface + faint grid + live green crosshair + per-frame readout as
 * the iOS companion, extended with a multi-finger readout so every gesture is
 * screenshot-verifiable.
 *
 * @param fullBleed when true (landscape), the whole rectangle is the pad with
 *   square corners; when false (portrait) it is a rounded inset card.
 */
@Composable
fun TouchpadSurface(
    deviceName: String,
    onEvent: (TouchpadEvent) -> Unit,
    modifier: Modifier = Modifier,
    fullBleed: Boolean = false
) {
    var state by remember { mutableStateOf(TouchpadState()) }
    val engine = remember {
        GestureEngine(emit = onEvent, onState = { state = it })
    }

    // Frame clock drives scroll inertia after the fingers lift.
    LaunchedEffect(engine) {
        while (true) {
            withFrameMillis { now ->
                if (engine.isInertiaActive) engine.tick(now)
            }
        }
    }

    val shape = RoundedCornerShape(if (fullBleed) 0.dp else 28.dp)

    Box(
        modifier = modifier
            .testTag("touchpad.surface")
            .semantics { contentDescription = "Touchpad" }
            .background(
                brush = Brush.verticalGradient(
                    listOf(MouserColors.SurfaceTop, MouserColors.SurfaceBottom)
                ),
                shape = shape
            )
            .border(width = 1.dp, color = MouserColors.Hairline, shape = shape)
            .pointerInput(Unit) {
                awaitPointerEventScope {
                    while (true) {
                        val event = awaitPointerEvent(PointerEventPass.Main)
                        val now = System.currentTimeMillis()
                        val active = event.changes
                            .filter { it.pressed }
                            .map { change ->
                                PointerSample(
                                    id = change.id.value,
                                    position = change.position,
                                    pressure = change.pressure
                                )
                            }
                        engine.onPointersChanged(active, now)
                        // Consume so parent containers don't also scroll.
                        event.changes.forEach { it.consume() }
                    }
                }
            }
    ) {
        TouchpadGrid()
        if (state.isTouching && state.touches.isNotEmpty()) {
            state.touches.forEach { TouchpadCrosshair(it) }
        } else {
            IdleHint(deviceName, Modifier.align(Alignment.Center))
        }
        TouchpadReadout(
            state = state,
            modifier = Modifier
                .align(Alignment.TopStart)
                .padding(14.dp)
        )
    }
}

/** Faint grid identical in feel to the iOS `Canvas` grid (36px step). */
@Composable
private fun TouchpadGrid() {
    Canvas(modifier = Modifier.fillMaxSize()) {
        val step = 36.dp.toPx()
        var x = step
        while (x < size.width) {
            drawLine(MouserColors.GridLine, Offset(x, 0f), Offset(x, size.height), 1f)
            x += step
        }
        var y = step
        while (y < size.height) {
            drawLine(MouserColors.GridLine, Offset(0f, y), Offset(size.width, y), 1f)
            y += step
        }
    }
}
