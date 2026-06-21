package ai.unlikeother.mouser.companion

import androidx.compose.animation.core.LinearEasing
import androidx.compose.animation.core.animateFloat
import androidx.compose.animation.core.animateFloatAsState
import androidx.compose.animation.core.infiniteRepeatable
import androidx.compose.animation.core.rememberInfiniteTransition
import androidx.compose.animation.core.tween
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.fillMaxHeight
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.Icon
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.layout.layout
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp

/**
 * Mac-style clipboard transfer "wait" indicator (§7.7 wait indicator) — the
 * companion's analogue of the macOS file-copy sheet: a peer + content label, a
 * rounded determinate progress bar, and a percent readout.
 *
 * Fed by a [ClipboardTransfer] (mock today; `ClipboardEngine::progress(hash)` once
 * the FFI lands — see [ClipboardUiState]). When [ClipboardTransfer.fraction] is 0
 * the bar shows an indeterminate "preparing…" sweep (a pull that hasn't received
 * its first chunk); otherwise it fills to the fraction. Renders nothing when there
 * is no transfer.
 */
@Composable
fun ClipboardWaitIndicator(transfer: ClipboardTransfer?, modifier: Modifier = Modifier) {
    if (transfer == null) return

    Column(
        modifier = modifier
            .fillMaxWidth()
            .background(MouserColors.Panel, RoundedCornerShape(14.dp))
            .padding(horizontal = 16.dp, vertical = 14.dp)
            .testTag("clipboard.progress")
    ) {
        Row(verticalAlignment = Alignment.CenterVertically) {
            Icon(
                imageVector = if (transfer.incoming) ClipboardIcons.ContentPaste else ClipboardIcons.ContentCopy,
                contentDescription = null,
                tint = MouserColors.Accent,
                modifier = Modifier.size(20.dp)
            )
            Spacer(modifier = Modifier.width(10.dp))
            Text(
                text = transferTitle(transfer),
                color = MouserColors.OnSurface,
                fontSize = 14.sp,
                fontWeight = FontWeight.SemiBold
            )
            Spacer(modifier = Modifier.weight(1f))
            Text(
                text = if (transfer.fraction <= 0f) "…" else "${transfer.percent}%",
                color = MouserColors.OnSurfaceDim,
                fontFamily = FontFamily.Monospace,
                fontSize = 13.sp,
                modifier = Modifier.testTag("clipboard.progress.percent")
            )
        }
        Spacer(modifier = Modifier.height(10.dp))
        if (transfer.fraction <= 0f) {
            IndeterminateBar()
        } else {
            DeterminateBar(fraction = transfer.fraction)
        }
        Spacer(modifier = Modifier.height(8.dp))
        Text(
            text = byteSubtitle(transfer),
            color = MouserColors.OnSurfaceFaint,
            fontFamily = FontFamily.Monospace,
            fontSize = 11.sp
        )
    }
}

/** A rounded determinate track + fill, the macOS progress-bar look. */
@Composable
private fun DeterminateBar(fraction: Float) {
    // Smoothly animate to the target fraction like the Mac copy sheet.
    val animated by animateFloatAsState(
        targetValue = fraction.coerceIn(0f, 1f),
        animationSpec = tween(durationMillis = 220),
        label = "clipboardProgress"
    )
    Box(
        modifier = Modifier
            .fillMaxWidth()
            .height(8.dp)
            .clip(RoundedCornerShape(4.dp))
            .background(MouserColors.ChipIdle)
    ) {
        Box(
            modifier = Modifier
                .fillMaxHeight()
                // Fraction of the parent width, computed in layout so it tracks any size.
                .layout { measurable, constraints ->
                    val w = (constraints.maxWidth * animated).toInt().coerceAtLeast(0)
                    val placeable = measurable.measure(constraints.copy(minWidth = w, maxWidth = w))
                    layout(placeable.width, placeable.height) { placeable.place(0, 0) }
                }
                .clip(RoundedCornerShape(4.dp))
                .background(MouserColors.Accent)
        )
    }
}

/** An indeterminate sweep for the "preparing…" phase (no bytes yet). */
@Composable
private fun IndeterminateBar() {
    val transition = rememberInfiniteTransition(label = "clipboardIndeterminate")
    val start by transition.animateFloat(
        initialValue = -0.35f,
        targetValue = 1.0f,
        animationSpec = infiniteRepeatable(
            animation = tween(durationMillis = 1100, easing = LinearEasing)
        ),
        label = "sweep"
    )
    Box(
        modifier = Modifier
            .fillMaxWidth()
            .height(8.dp)
            .clip(RoundedCornerShape(4.dp))
            .background(MouserColors.ChipIdle)
    ) {
        val segment = 0.35f
        Box(
            modifier = Modifier
                .fillMaxHeight()
                .layout { measurable, constraints ->
                    val full = constraints.maxWidth
                    val segW = (full * segment).toInt().coerceAtLeast(1)
                    val offset = (full * start).toInt()
                    val placeable = measurable.measure(
                        constraints.copy(minWidth = segW, maxWidth = segW)
                    )
                    layout(full, placeable.height) {
                        placeable.place(offset.coerceIn(-segW, full), 0)
                    }
                }
                .clip(RoundedCornerShape(4.dp))
                .background(MouserColors.Accent)
        )
    }
}

private fun transferTitle(t: ClipboardTransfer): String {
    val verb = if (t.incoming) "Receiving" else "Sending"
    val prep = if (t.incoming) "from" else "to"
    return "$verb ${t.kind} $prep ${t.peerName}"
}

private fun byteSubtitle(t: ClipboardTransfer): String {
    val done = formatBytes(t.transferredBytes)
    val total = formatBytes(t.totalBytes)
    return "$done of $total"
}

/** Compact binary byte formatter (KB/MB) for the subtitle. */
private fun formatBytes(bytes: Long): String {
    if (bytes < 1024) return "$bytes B"
    val kb = bytes / 1024.0
    if (kb < 1024) return "%.0f KB".format(kb)
    val mb = kb / 1024.0
    return "%.1f MB".format(mb)
}
