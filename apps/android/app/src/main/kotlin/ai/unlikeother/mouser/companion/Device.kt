package ai.unlikeother.mouser.companion

import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Laptop
import androidx.compose.material.icons.filled.PersonalVideo
import androidx.compose.material.icons.filled.Terminal
import androidx.compose.ui.graphics.vector.ImageVector

/**
 * A target computer the companion can drive.
 *
 * Parity with the iOS `Device` enum: in the real app these are discovered
 * cluster peers (architecture §9); for this UI/gesture spike they are a fixed
 * set so the device-selector row and the "Controlling: <device>" banner have
 * something to bind to. Glyphs are generic (not OS logos), matching iOS.
 */
enum class Device(val displayName: String) {
    MAC("Mac"),
    WINDOWS("Windows"),
    LINUX("Linux");

    val icon: ImageVector
        get() = when (this) {
            MAC -> Icons.Filled.Laptop
            WINDOWS -> Icons.Filled.PersonalVideo
            LINUX -> Icons.Filled.Terminal
        }
}
