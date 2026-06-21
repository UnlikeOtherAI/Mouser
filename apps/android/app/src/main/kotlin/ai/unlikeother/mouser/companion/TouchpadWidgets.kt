package ai.unlikeother.mouser.companion

import androidx.compose.foundation.Canvas
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.TouchApp
import androidx.compose.material3.Icon
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.graphics.drawscope.Stroke
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp

/** Live crosshair at a touch point — parity with iOS `TouchpadCrosshair`. */
@Composable
fun TouchpadCrosshair(point: Offset) {
    Canvas(modifier = Modifier.fillMaxSize()) {
        val r = 32.dp.toPx()
        drawCircle(MouserColors.Live.copy(alpha = 0.18f), radius = r, center = point)
        drawCircle(
            MouserColors.Live.copy(alpha = 0.8f),
            radius = r,
            center = point,
            style = Stroke(width = 1.5.dp.toPx())
        )
        drawCircle(MouserColors.Live, radius = 5.dp.toPx(), center = point)
        val arm = 11.dp.toPx()
        drawLine(
            MouserColors.Live.copy(alpha = 0.6f),
            Offset(point.x, point.y - arm), Offset(point.x, point.y + arm), 1f
        )
        drawLine(
            MouserColors.Live.copy(alpha = 0.6f),
            Offset(point.x - arm, point.y), Offset(point.x + arm, point.y), 1f
        )
    }
}

/** Centered idle hint — parity with iOS `idleHint`. */
@Composable
fun IdleHint(deviceName: String, modifier: Modifier = Modifier) {
    Column(
        modifier = modifier,
        horizontalAlignment = Alignment.CenterHorizontally,
        verticalArrangement = Arrangement.spacedBy(8.dp)
    ) {
        Icon(
            imageVector = Icons.Filled.TouchApp,
            contentDescription = null,
            tint = MouserColors.OnSurfaceFaint
        )
        Text(
            text = "Drag to move $deviceName",
            color = MouserColors.OnSurfaceFaint,
            fontSize = 14.sp
        )
    }
}

/**
 * The live readout (parity with iOS `readout`, extended for the richer gesture
 * set). Shows the per-frame move/scroll deltas, pinch magnify/rotate, the
 * capacitive pressure approximation, and the last discrete action so every
 * gesture is verifiable in a screenshot.
 */
@Composable
fun TouchpadReadout(state: TouchpadState, modifier: Modifier = Modifier) {
    val live = state.isTouching
    Column(modifier = modifier.testTag("touchpad.readout")) {
        Text(
            text = "TOUCHPAD",
            color = MouserColors.OnSurfaceFaint,
            fontSize = 11.sp,
            fontWeight = FontWeight.SemiBold,
            letterSpacing = 1.5.sp
        )
        Mono("Δ %+.0f, %+.0f".format(state.lastMove.x, state.lastMove.y), live)
        Mono("scroll %+.0f, %+.0f".format(state.lastScroll.x, state.lastScroll.y), live)
        Mono(
            "pinch %.2fx  rot %+.0f°".format(state.magnification, state.rotationDegrees),
            live
        )
        Mono("pressure %.2f".format(state.pressure), live)
        Text(
            text = state.lastAction,
            color = MouserColors.Live,
            fontFamily = FontFamily.Monospace,
            fontSize = 13.sp,
            fontWeight = FontWeight.SemiBold,
            modifier = Modifier.testTag("touchpad.action")
        )
    }
}

@Composable
private fun Mono(text: String, live: Boolean) {
    Text(
        text = text,
        color = if (live) MouserColors.Live else MouserColors.OnSurfaceDim,
        fontFamily = FontFamily.Monospace,
        fontSize = 13.sp
    )
}
