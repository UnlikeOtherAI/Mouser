package ai.unlikeother.mouser.companion

import androidx.compose.runtime.Composable
import androidx.compose.ui.graphics.vector.ImageVector

/**
 * A target computer the companion can drive.
 *
 * Parity with the iOS `Device` enum: in the real app these are discovered
 * cluster peers (architecture §9); for this UI/gesture spike they are a fixed
 * set so the device-selector row and the "Controlling: <device>" banner have
 * something to bind to. Glyphs are generic (not OS logos), matching iOS, and are
 * local vectors ([MouserIcons]) so the app needn't pull material-icons-extended.
 */
enum class Device(val displayName: String) {
    MAC("Mac"),
    WINDOWS("Windows"),
    LINUX("Linux");

    val icon: ImageVector
        @Composable get() = when (this) {
            MAC -> MouserIcons.Laptop
            WINDOWS -> MouserIcons.PersonalVideo
            LINUX -> MouserIcons.Terminal
        }
}
