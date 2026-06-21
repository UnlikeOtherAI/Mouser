package ai.unlikeother.mouser.companion

import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.SettingsRemote
import androidx.compose.material3.Icon
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.AnnotatedString
import androidx.compose.ui.text.SpanStyle
import androidx.compose.ui.text.buildAnnotatedString
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.withStyle
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp

/**
 * Thin persistent "Controlling: <device>" banner (architecture §9), sitting
 * between the touchpad and the device-selector row. Parity with iOS
 * `ControllingBanner`.
 */
@Composable
fun ControllingBanner(deviceName: String, modifier: Modifier = Modifier) {
    Row(
        modifier = modifier
            .fillMaxWidth()
            .background(MouserColors.Panel, RoundedCornerShape(10.dp))
            .padding(horizontal = 14.dp, vertical = 9.dp)
            .testTag("controlling.banner")
            .semantics { contentDescription = "Controlling $deviceName" },
        verticalAlignment = Alignment.CenterVertically
    ) {
        Box(
            modifier = Modifier
                .size(8.dp)
                .background(MouserColors.Live, CircleShape)
        )
        Text(
            text = controllingLabel(deviceName),
            fontSize = 13.sp,
            modifier = Modifier.padding(start = 8.dp)
        )
        Box(modifier = Modifier.weight(1f))
        Icon(
            imageVector = Icons.Filled.SettingsRemote,
            contentDescription = null,
            tint = MouserColors.OnSurfaceDim
        )
    }
}

private fun controllingLabel(deviceName: String): AnnotatedString = buildAnnotatedString {
    withStyle(SpanStyle(color = MouserColors.OnSurfaceDim)) { append("Controlling: ") }
    withStyle(SpanStyle(color = MouserColors.OnSurface, fontWeight = FontWeight.Bold)) {
        append(deviceName)
    }
}
