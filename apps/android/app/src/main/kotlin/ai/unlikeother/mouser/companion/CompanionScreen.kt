package ai.unlikeother.mouser.companion

import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ColumnScope
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.imePadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.safeDrawingPadding
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.BasicTextField
import androidx.compose.material3.Icon
import androidx.compose.material3.LocalTextStyle
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.compose.LifecycleEventEffect
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.focus.focusRequester
import androidx.compose.ui.graphics.SolidColor
import androidx.compose.ui.platform.LocalConfiguration
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.input.KeyboardCapitalization
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.text.input.TextFieldValue
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import android.content.res.Configuration
import androidx.compose.foundation.text.KeyboardOptions

/**
 * Root companion screen (brief: Mobile Companion App).
 *
 * Portrait: touchpad on top, a "Controlling: <device>" banner, the device
 * selector (Mac / Windows / Linux), then a focused capture field that summons
 * the NATIVE Android soft keyboard. Landscape: the ENTIRE screen is one
 * full-bleed touchpad with no keyboard chrome.
 *
 * Gesture events are routed to [Haptics] for tactile feedback (and will later
 * become wire datagrams, communication-interface §6).
 *
 * Lifecycle (audit R2 HIGH): a [LifecycleEventEffect] mirrors the activity-level
 * observer for the Compose tree — on `ON_STOP` it drives [CompanionSession.onStop]
 * (stop the frame loop / streaming, yield ownership) and on `ON_RESUME`
 * [CompanionSession.onResume] (reconnect). The touchpad's frame loop keys off
 * [CompanionSession.isForeground] so it genuinely stops while backgrounded.
 */
@Composable
fun CompanionScreen(
    haptics: Haptics?,
    session: CompanionSession = remember { CompanionSession() },
    mouser: MouserClient = remember { MouserClient() }
) {
    var selected by remember { mutableStateOf(Device.MAC) }
    var tab by remember { mutableStateOf(CompanionTab.TOUCHPAD) }
    // Clipboard UI state (mock today; bound to ClipboardEngine once the FFI lands —
    // see ClipboardUiState). Hoisted here so the host owns it across tab switches.
    val clipboard = remember { ClipboardUiState() }
    // Every recognised gesture drives local haptics AND, while connected, forwards
    // over the wire through the mouser-ffi bridge (sendPointerMoved/button/scroll).
    val onEvent: (TouchpadEvent) -> Unit = remember(haptics, mouser) {
        { event ->
            haptics?.feedback(event)
            mouser.onEvent(event)
        }
    }

    // Compose-side lifecycle hooks (the activity also observes at process scope).
    LifecycleEventEffect(Lifecycle.Event.ON_STOP) { session.onStop() }
    LifecycleEventEffect(Lifecycle.Event.ON_RESUME) { session.onResume() }

    val isLandscape =
        LocalConfiguration.current.orientation == Configuration.ORIENTATION_LANDSCAPE

    Box(
        modifier = Modifier
            .fillMaxSize()
            .background(MouserColors.Background)
    ) {
        if (isLandscape) {
            // Landscape stays a single full-bleed touchpad (no chrome).
            LandscapePad(
                deviceName = selected.displayName,
                onEvent = onEvent,
                isForeground = session.isForeground
            )
        } else {
            PortraitLayout(
                selected = selected,
                onSelect = { selected = it },
                onEvent = onEvent,
                onKey = mouser::sendCharacter,
                isForeground = session.isForeground,
                tab = tab,
                onTabChange = { tab = it },
                clipboard = clipboard
            )
        }
    }
}

/** Portrait top-level destinations: the remote touchpad or the clipboard. */
enum class CompanionTab(val label: String) { TOUCHPAD("Touchpad"), CLIPBOARD("Clipboard") }

/** Landscape: the whole screen is the touchpad (brief: full-screen trackpad). */
@Composable
private fun LandscapePad(
    deviceName: String,
    onEvent: (TouchpadEvent) -> Unit,
    isForeground: Boolean
) {
    TouchpadSurface(
        deviceName = deviceName,
        onEvent = onEvent,
        isForeground = isForeground,
        fullBleed = true,
        modifier = Modifier
            .fillMaxSize()
            .testTag("companion.landscape")
    )
}

@Composable
private fun PortraitLayout(
    selected: Device,
    onSelect: (Device) -> Unit,
    onEvent: (TouchpadEvent) -> Unit,
    onKey: (Char) -> Unit,
    isForeground: Boolean,
    tab: CompanionTab,
    onTabChange: (CompanionTab) -> Unit,
    clipboard: ClipboardUiState
) {
    Column(
        modifier = Modifier
            .fillMaxSize()
            .safeDrawingPadding()
            .imePadding()
            .padding(horizontal = 14.dp, vertical = 10.dp)
            .testTag("companion.portrait")
    ) {
        TabSwitcher(current = tab, onSelect = onTabChange)
        Spacer(modifier = Modifier.height(12.dp))
        when (tab) {
            CompanionTab.TOUCHPAD -> TouchpadTab(
                selected = selected,
                onSelect = onSelect,
                onEvent = onEvent,
                isForeground = isForeground,
                onKey = onKey
            )
            // Mac-style wait indicator (mock transfer) + the §7.7 settings section.
            CompanionTab.CLIPBOARD -> ClipboardScreen(
                modifier = Modifier.weight(1f),
                state = clipboard
            )
        }
    }
}

/** The original touchpad stack: pad, controlling banner, device row, capture field. */
@Composable
private fun ColumnScope.TouchpadTab(
    selected: Device,
    onSelect: (Device) -> Unit,
    onEvent: (TouchpadEvent) -> Unit,
    isForeground: Boolean,
    onKey: (Char) -> Unit
) {
    TouchpadSurface(
        deviceName = selected.displayName,
        onEvent = onEvent,
        isForeground = isForeground,
        modifier = Modifier
            .fillMaxWidth()
            .weight(1f)
    )
    Spacer(modifier = Modifier.height(12.dp))
    ControllingBanner(deviceName = selected.displayName)
    Spacer(modifier = Modifier.height(12.dp))
    DeviceSelectorRow(selected = selected, onSelect = onSelect)
    Spacer(modifier = Modifier.height(12.dp))
    CaptureField(onKey = onKey)
}

/** Two-tab segmented switch between the touchpad and the clipboard screen. */
@Composable
private fun TabSwitcher(current: CompanionTab, onSelect: (CompanionTab) -> Unit) {
    Row(
        modifier = Modifier
            .fillMaxWidth()
            .testTag("companion.tabs"),
        horizontalArrangement = androidx.compose.foundation.layout.Arrangement.spacedBy(8.dp)
    ) {
        CompanionTab.entries.forEach { entry ->
            val isSelected = entry == current
            Text(
                text = entry.label,
                color = if (isSelected) androidx.compose.ui.graphics.Color.White else MouserColors.OnSurfaceDim,
                fontSize = 14.sp,
                modifier = Modifier
                    .weight(1f)
                    .background(
                        if (isSelected) MouserColors.Accent else MouserColors.ChipIdle,
                        RoundedCornerShape(10.dp)
                    )
                    .clickable { onSelect(entry) }
                    .padding(vertical = 10.dp)
                    .testTag("companion.tab.${entry.name}"),
                textAlign = androidx.compose.ui.text.style.TextAlign.Center
            )
        }
    }
}

/**
 * Bottom capture field. Its only job in this spike is to hold keyboard focus so
 * the system soft keyboard appears (its keystrokes will later become HID
 * `KeyEvent`s, architecture §9). Parity with iOS `captureField`.
 */
@Composable
private fun CaptureField(onKey: (Char) -> Unit) {
    var value by remember { mutableStateOf(TextFieldValue("")) }
    val focusRequester = remember { FocusRequester() }

    // Raise the native keyboard so the signature split layout is visible
    // immediately (and so a screenshot proves the keyboard renders).
    LaunchedEffect(Unit) {
        focusRequester.requestFocus()
    }

    Row(
        modifier = Modifier
            .fillMaxWidth()
            .background(MouserColors.Panel, RoundedCornerShape(12.dp))
            .padding(horizontal = 14.dp, vertical = 12.dp),
        verticalAlignment = Alignment.CenterVertically
    ) {
        Icon(
            imageVector = MouserIcons.Keyboard,
            contentDescription = null,
            tint = MouserColors.OnSurfaceDim
        )
        Box(modifier = Modifier.weight(1f).padding(start = 8.dp)) {
            if (value.text.isEmpty()) {
                Text(
                    text = "Type to send keystrokes…",
                    color = MouserColors.OnSurfaceDim,
                    fontSize = 16.sp
                )
            }
            BasicTextField(
                value = value,
                onValueChange = { next ->
                    // Forward each newly appended character as a HID keystroke over
                    // the wire (no-op while disconnected). Only growth is treated as
                    // typing; edits/deletes don't emit. The field itself stays a thin
                    // capture surface — its text isn't the source of truth.
                    if (next.text.length > value.text.length) {
                        next.text.substring(value.text.length).forEach(onKey)
                    }
                    value = next
                },
                singleLine = true,
                cursorBrush = SolidColor(MouserColors.Live),
                textStyle = LocalTextStyle.current.copy(
                    color = MouserColors.OnSurface,
                    fontSize = 16.sp
                ),
                keyboardOptions = KeyboardOptions(
                    capitalization = KeyboardCapitalization.None,
                    autoCorrectEnabled = false,
                    keyboardType = KeyboardType.Ascii,
                    imeAction = ImeAction.Send
                ),
                modifier = Modifier
                    .fillMaxWidth()
                    .focusRequester(focusRequester)
                    .testTag("keyboard.field")
            )
        }
    }
}
