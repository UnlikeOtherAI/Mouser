package ai.unlikeother.mouser.companion

import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.darkColorScheme
import androidx.compose.runtime.Composable
import androidx.compose.ui.graphics.Color

/**
 * Shared palette, mirroring the iOS companion's dark look.
 *
 * iOS uses `Color.accentColor` (the system blue) for the selected device chip
 * and a green accent for the live touch crosshair / readout. We reproduce both.
 */
object MouserColors {
    val Background = Color(0xFF000000)
    val SurfaceTop = Color(0xFF292929)      // white 0.16
    val SurfaceBottom = Color(0xFF1A1A1A)   // white 0.10
    val Panel = Color(0xFF242424)           // white 0.14
    val ChipIdle = Color(0xFF2E2E2E)        // white 0.18
    val Accent = Color(0xFF0A84FF)          // iOS system blue (dark)
    val Live = Color(0xFF34C759)            // iOS system green
    val GridLine = Color(0x0AFFFFFF)        // white 0.04
    val Hairline = Color(0x1AFFFFFF)        // white 0.10
    val OnSurface = Color(0xFFFFFFFF)
    val OnSurfaceDim = Color(0x80FFFFFF)    // white 0.5
    val OnSurfaceFaint = Color(0x5AFFFFFF)  // white ~0.35
}

@Composable
fun MouserTheme(content: @Composable () -> Unit) {
    MaterialTheme(
        colorScheme = darkColorScheme(
            primary = MouserColors.Accent,
            background = MouserColors.Background,
            surface = MouserColors.Panel
        ),
        content = content
    )
}
