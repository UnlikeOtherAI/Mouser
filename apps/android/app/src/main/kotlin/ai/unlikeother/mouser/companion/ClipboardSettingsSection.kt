package ai.unlikeother.mouser.companion

import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.Icon
import androidx.compose.material3.Switch
import androidx.compose.material3.SwitchDefaults
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp

/**
 * The Clipboard settings section (§7.7) — the companion's editor for one device's
 * `ClipboardSettings`. Master on/off, per-format gates (text / images / files), an
 * auto-sync size cap, prefer-native-Apple, and the sync direction.
 *
 * Bound to a [ClipboardUiState]; today edits stay in-process (mock), and the
 * comment on [ClipboardUiState.updateSettings] notes the call becomes
 * `ClipboardEngine::set_settings` once the FFI lands. Sub-controls dim/disable when
 * the master switch is off, mirroring the crate's gate (`can_offer`/`can_receive`
 * are false while `shared_clipboard` is false).
 *
 * Presented as an embeddable section (so it can live in a settings screen or a
 * sheet); [ClipboardScreen] wraps it as a standalone scrollable screen.
 */
@Composable
fun ClipboardSettingsSection(state: ClipboardUiState, modifier: Modifier = Modifier) {
    val s = state.settings
    val enabled = s.sharedClipboard

    Column(
        modifier = modifier
            .fillMaxWidth()
            .testTag("clipboard.settings")
    ) {
        SectionHeader(icon = ClipboardIcons.Tune, title = "Shared Clipboard")
        Spacer(modifier = Modifier.height(10.dp))

        // Master switch.
        SettingRow(
            icon = ClipboardIcons.Sync,
            title = "Shared clipboard",
            subtitle = "Sync copy & paste across your devices",
            checked = s.sharedClipboard,
            onCheckedChange = { state.updateSettings(s.copy(sharedClipboard = it)) },
            testTag = "clipboard.master"
        )

        Spacer(modifier = Modifier.height(14.dp))
        GroupLabel("Formats")
        SettingRow(
            icon = ClipboardIcons.TextFields,
            title = "Text",
            subtitle = "Plain text, HTML & rich text",
            checked = s.syncText,
            enabled = enabled,
            onCheckedChange = { state.updateSettings(s.copy(syncText = it)) },
            testTag = "clipboard.text"
        )
        SettingRow(
            icon = ClipboardIcons.Image,
            title = "Images",
            subtitle = "PNG image data",
            checked = s.syncImages,
            enabled = enabled,
            onCheckedChange = { state.updateSettings(s.copy(syncImages = it)) },
            testTag = "clipboard.images"
        )
        SettingRow(
            icon = ClipboardIcons.InsertDriveFile,
            title = "Files",
            subtitle = "File references (uri list)",
            checked = s.syncFiles,
            enabled = enabled,
            onCheckedChange = { state.updateSettings(s.copy(syncFiles = it)) },
            testTag = "clipboard.files"
        )

        Spacer(modifier = Modifier.height(14.dp))
        GroupLabel("Behaviour")
        DirectionRow(
            current = s.direction,
            enabled = enabled,
            onSelect = { state.updateSettings(s.copy(direction = it)) }
        )
        Spacer(modifier = Modifier.height(8.dp))
        MaxSizeRow(
            current = s.maxAutoSyncBytes,
            enabled = enabled,
            onSelect = { state.updateSettings(s.copy(maxAutoSyncBytes = it)) }
        )
        Spacer(modifier = Modifier.height(8.dp))
        SettingRow(
            icon = ClipboardIcons.Devices,
            title = "Prefer Apple Universal Clipboard",
            subtitle = "Let macOS/iOS carry copy & paste between Apple devices",
            checked = s.preferNativeApple,
            enabled = enabled,
            onCheckedChange = { state.updateSettings(s.copy(preferNativeApple = it)) },
            testTag = "clipboard.preferNative"
        )
    }
}

@Composable
private fun SectionHeader(icon: ImageVector, title: String) {
    Row(verticalAlignment = Alignment.CenterVertically) {
        Icon(imageVector = icon, contentDescription = null, tint = MouserColors.OnSurfaceDim)
        Spacer(modifier = Modifier.width(8.dp))
        Text(
            text = title,
            color = MouserColors.OnSurface,
            fontSize = 18.sp,
            fontWeight = FontWeight.Bold
        )
    }
}

@Composable
private fun GroupLabel(text: String) {
    Text(
        text = text.uppercase(),
        color = MouserColors.OnSurfaceFaint,
        fontSize = 11.sp,
        fontWeight = FontWeight.SemiBold,
        letterSpacing = 1.2.sp,
        modifier = Modifier.padding(start = 4.dp, bottom = 6.dp)
    )
}

@Composable
private fun SettingRow(
    icon: ImageVector,
    title: String,
    subtitle: String,
    checked: Boolean,
    onCheckedChange: (Boolean) -> Unit,
    testTag: String,
    enabled: Boolean = true
) {
    val contentTint = if (enabled) MouserColors.OnSurface else MouserColors.OnSurfaceFaint
    Row(
        modifier = Modifier
            .fillMaxWidth()
            .padding(vertical = 4.dp)
            .background(MouserColors.Panel, RoundedCornerShape(12.dp))
            .padding(horizontal = 14.dp, vertical = 12.dp)
            .testTag(testTag),
        verticalAlignment = Alignment.CenterVertically
    ) {
        Icon(
            imageVector = icon,
            contentDescription = null,
            tint = if (enabled) MouserColors.OnSurfaceDim else MouserColors.OnSurfaceFaint,
            modifier = Modifier.size(22.dp)
        )
        Spacer(modifier = Modifier.width(12.dp))
        Column(modifier = Modifier.weight(1f)) {
            Text(text = title, color = contentTint, fontSize = 15.sp, fontWeight = FontWeight.Medium)
            Text(text = subtitle, color = MouserColors.OnSurfaceFaint, fontSize = 12.sp)
        }
        Switch(
            checked = checked,
            onCheckedChange = onCheckedChange,
            enabled = enabled,
            colors = SwitchDefaults.colors(
                checkedThumbColor = Color.White,
                checkedTrackColor = MouserColors.Live,
                uncheckedThumbColor = MouserColors.OnSurfaceDim,
                uncheckedTrackColor = MouserColors.ChipIdle
            )
        )
    }
}

/** Segmented direction picker (both ways / send only / receive only). */
@Composable
private fun DirectionRow(
    current: SyncDirection,
    enabled: Boolean,
    onSelect: (SyncDirection) -> Unit
) {
    Column(
        modifier = Modifier
            .fillMaxWidth()
            .padding(vertical = 4.dp)
            .background(MouserColors.Panel, RoundedCornerShape(12.dp))
            .padding(horizontal = 14.dp, vertical = 12.dp)
            .testTag("clipboard.direction")
    ) {
        Text(
            text = "Direction",
            color = if (enabled) MouserColors.OnSurface else MouserColors.OnSurfaceFaint,
            fontSize = 15.sp,
            fontWeight = FontWeight.Medium
        )
        Spacer(modifier = Modifier.height(10.dp))
        Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            DirectionChip(ClipboardIcons.SwapHoriz, SyncDirection.BIDIRECTIONAL, current, enabled, onSelect, Modifier.weight(1f))
            DirectionChip(ClipboardIcons.ArrowForward, SyncDirection.SEND_ONLY, current, enabled, onSelect, Modifier.weight(1f))
            DirectionChip(ClipboardIcons.ArrowBack, SyncDirection.RECEIVE_ONLY, current, enabled, onSelect, Modifier.weight(1f))
        }
    }
}

@Composable
private fun DirectionChip(
    icon: ImageVector,
    value: SyncDirection,
    current: SyncDirection,
    enabled: Boolean,
    onSelect: (SyncDirection) -> Unit,
    modifier: Modifier = Modifier
) {
    val selected = value == current
    val bg = when {
        !enabled -> MouserColors.ChipIdle
        selected -> MouserColors.Accent
        else -> MouserColors.ChipIdle
    }
    val tint = when {
        !enabled -> MouserColors.OnSurfaceFaint
        selected -> Color.White
        else -> MouserColors.OnSurfaceDim
    }
    Column(
        modifier = modifier
            .background(bg, RoundedCornerShape(10.dp))
            .let { if (enabled) it.clickable { onSelect(value) } else it }
            .padding(vertical = 10.dp)
            .testTag("clipboard.direction.${value.name}"),
        horizontalAlignment = Alignment.CenterHorizontally
    ) {
        Icon(imageVector = icon, contentDescription = null, tint = tint, modifier = Modifier.size(20.dp))
        Spacer(modifier = Modifier.height(4.dp))
        Text(text = value.label, color = tint, fontSize = 11.sp)
    }
}

/**
 * Max auto-sync size cap (`max_auto_sync_bytes`, `0` = unlimited). A small set of
 * presets keeps the spike UI simple; the engine just wants a byte count.
 */
@Composable
private fun MaxSizeRow(
    current: Long,
    enabled: Boolean,
    onSelect: (Long) -> Unit
) {
    val presets = listOf(
        0L to "Unlimited",
        5L * 1024 * 1024 to "5 MB",
        25L * 1024 * 1024 to "25 MB",
        100L * 1024 * 1024 to "100 MB"
    )
    Column(
        modifier = Modifier
            .fillMaxWidth()
            .padding(vertical = 4.dp)
            .background(MouserColors.Panel, RoundedCornerShape(12.dp))
            .padding(horizontal = 14.dp, vertical = 12.dp)
            .testTag("clipboard.maxSize")
    ) {
        Text(
            text = "Auto-sync limit",
            color = if (enabled) MouserColors.OnSurface else MouserColors.OnSurfaceFaint,
            fontSize = 15.sp,
            fontWeight = FontWeight.Medium
        )
        Text(
            text = "Skip eager pull above this size",
            color = MouserColors.OnSurfaceFaint,
            fontSize = 12.sp
        )
        Spacer(modifier = Modifier.height(10.dp))
        Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            presets.forEach { (bytes, label) ->
                val selected = bytes == current
                Text(
                    text = label,
                    color = if (selected) Color.White else MouserColors.OnSurfaceDim,
                    fontSize = 12.sp,
                    fontWeight = if (selected) FontWeight.SemiBold else FontWeight.Normal,
                    modifier = Modifier
                        .background(
                            if (selected && enabled) MouserColors.Accent else MouserColors.ChipIdle,
                            RoundedCornerShape(8.dp)
                        )
                        .let { if (enabled) it.clickable { onSelect(bytes) } else it }
                        .padding(horizontal = 12.dp, vertical = 7.dp)
                        .testTag("clipboard.maxSize.$bytes")
                )
            }
        }
    }
}
