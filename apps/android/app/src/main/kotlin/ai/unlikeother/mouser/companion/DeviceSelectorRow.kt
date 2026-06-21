package ai.unlikeother.mouser.companion

import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.RowScope
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.Icon
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp

/**
 * Quick device-selection row (brief: "Tap Mac / Windows / Linux — instant
 * ownership transfer"). In the real app each tap issues an `OwnershipRequest`
 * (architecture §9); here it updates the selection. Parity with iOS
 * `DeviceSelectorRow`.
 */
@Composable
fun DeviceSelectorRow(
    selected: Device,
    onSelect: (Device) -> Unit,
    modifier: Modifier = Modifier
) {
    Row(
        modifier = modifier
            .fillMaxWidth()
            .testTag("device.selector"),
        horizontalArrangement = Arrangement.spacedBy(10.dp)
    ) {
        Device.entries.forEach { device ->
            Chip(device = device, isSelected = device == selected, onSelect = onSelect)
        }
    }
}

@Composable
private fun RowScope.Chip(device: Device, isSelected: Boolean, onSelect: (Device) -> Unit) {
    val shape = RoundedCornerShape(12.dp)
    Row(
        modifier = Modifier
            .weight(1f)
            .background(if (isSelected) MouserColors.Accent else MouserColors.ChipIdle, shape)
            .border(
                width = 1.dp,
                color = if (isSelected) Color.White.copy(alpha = 0.25f) else Color.Transparent,
                shape = shape
            )
            .clickable { onSelect(device) }
            .padding(vertical = 11.dp)
            .testTag("device.chip.${device.displayName}"),
        horizontalArrangement = Arrangement.Center,
        verticalAlignment = Alignment.CenterVertically
    ) {
        val tint = if (isSelected) Color.White else MouserColors.OnSurfaceDim
        Icon(imageVector = device.icon, contentDescription = null, tint = tint)
        Text(
            text = device.displayName,
            color = tint,
            fontSize = 15.sp,
            fontWeight = FontWeight.SemiBold,
            modifier = Modifier.padding(start = 6.dp)
        )
    }
}
