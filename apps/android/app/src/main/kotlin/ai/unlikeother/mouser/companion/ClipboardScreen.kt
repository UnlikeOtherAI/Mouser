package ai.unlikeother.mouser.companion

import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.safeDrawingPadding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.runtime.Composable
import androidx.compose.runtime.remember
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.unit.dp

/**
 * Standalone Clipboard screen: the Mac-style in-flight [ClipboardWaitIndicator]
 * (when a transfer is running) above the [ClipboardSettingsSection].
 *
 * This is the screen the companion will navigate to from a settings entry; it owns
 * a [ClipboardUiState] (mock data) so it renders without any engine. Once the
 * `mouser-ffi` surface lands, the host hoists the state and feeds it from
 * `ClipboardEngine` (settings + `progress`) — see [ClipboardUiState].
 */
@Composable
fun ClipboardScreen(
    modifier: Modifier = Modifier,
    state: ClipboardUiState = remember { ClipboardUiState() }
) {
    Column(
        modifier = modifier
            .fillMaxSize()
            .background(MouserColors.Background)
            .safeDrawingPadding()
            .verticalScroll(rememberScrollState())
            .padding(horizontal = 16.dp, vertical = 14.dp)
            .testTag("clipboard.screen")
    ) {
        state.transfer?.let { transfer ->
            ClipboardWaitIndicator(transfer = transfer)
            Spacer(modifier = Modifier.height(18.dp))
        }
        ClipboardSettingsSection(state = state)
    }
}
