package ai.unlikeother.mouser.companion

import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
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
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Keyboard
import androidx.compose.material3.Icon
import androidx.compose.material3.LocalTextStyle
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
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
 */
@Composable
fun CompanionScreen(haptics: Haptics?) {
    var selected by remember { mutableStateOf(Device.MAC) }
    val onEvent: (TouchpadEvent) -> Unit = remember(haptics) {
        { event -> haptics?.feedback(event) }
    }

    val isLandscape =
        LocalConfiguration.current.orientation == Configuration.ORIENTATION_LANDSCAPE

    Box(
        modifier = Modifier
            .fillMaxSize()
            .background(MouserColors.Background)
    ) {
        if (isLandscape) {
            LandscapePad(deviceName = selected.displayName, onEvent = onEvent)
        } else {
            PortraitLayout(
                selected = selected,
                onSelect = { selected = it },
                onEvent = onEvent
            )
        }
    }
}

/** Landscape: the whole screen is the touchpad (brief: full-screen trackpad). */
@Composable
private fun LandscapePad(deviceName: String, onEvent: (TouchpadEvent) -> Unit) {
    TouchpadSurface(
        deviceName = deviceName,
        onEvent = onEvent,
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
    onEvent: (TouchpadEvent) -> Unit
) {
    Column(
        modifier = Modifier
            .fillMaxSize()
            .safeDrawingPadding()
            .imePadding()
            .padding(horizontal = 14.dp, vertical = 10.dp)
            .testTag("companion.portrait")
    ) {
        TouchpadSurface(
            deviceName = selected.displayName,
            onEvent = onEvent,
            modifier = Modifier
                .fillMaxWidth()
                .weight(1f)
        )
        Spacer(modifier = Modifier.height(12.dp))
        ControllingBanner(deviceName = selected.displayName)
        Spacer(modifier = Modifier.height(12.dp))
        DeviceSelectorRow(selected = selected, onSelect = onSelect)
        Spacer(modifier = Modifier.height(12.dp))
        CaptureField()
    }
}

/**
 * Bottom capture field. Its only job in this spike is to hold keyboard focus so
 * the system soft keyboard appears (its keystrokes will later become HID
 * `KeyEvent`s, architecture §9). Parity with iOS `captureField`.
 */
@Composable
private fun CaptureField() {
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
            imageVector = Icons.Filled.Keyboard,
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
                onValueChange = { value = it },
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
