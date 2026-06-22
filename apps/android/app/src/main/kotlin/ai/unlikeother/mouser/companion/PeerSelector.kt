package ai.unlikeother.mouser.companion

import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.shape.CircleShape
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
 * The connection phase the [PeerSelector] reflects, so a tapped peer shows a
 * "connecting…" then "connected" affordance instead of silently dialing. Held by
 * the host composable; [PeerDiscovery] only supplies the peer set.
 */
sealed interface ConnectionUiState {
    data object Idle : ConnectionUiState
    data class Connecting(val peerId: String) : ConnectionUiState
    data class Connected(val peerId: String) : ConnectionUiState
    data class Failed(val peerId: String) : ConnectionUiState
}

/**
 * Discovered-peer list (brief: tap-to-connect). Renders the live `_mouser._udp`
 * peers [PeerDiscovery] publishes; tapping one calls back with the peer to dial
 * (the host runs [MouserClient.connect] off the main thread). The currently
 * connecting/connected peer is highlighted from [connection].
 *
 * Empty state shows a "searching…" line so the user knows discovery is live but
 * nothing has answered yet (parity with the device-selector row's always-present
 * chrome).
 */
@Composable
fun PeerSelector(
    peers: List<DiscoveredPeer>,
    connection: ConnectionUiState,
    onConnect: (DiscoveredPeer) -> Unit,
    modifier: Modifier = Modifier,
) {
    Column(
        modifier = modifier
            .fillMaxWidth()
            .testTag("peer.selector"),
        verticalArrangement = Arrangement.spacedBy(8.dp),
    ) {
        Text(
            text = if (peers.isEmpty()) "Searching for computers…" else "Computers on this network",
            color = MouserColors.OnSurfaceDim,
            fontSize = 12.sp,
        )
        if (peers.isEmpty()) {
            Spacer(modifier = Modifier.height(2.dp))
        } else {
            peers.forEach { peer ->
                PeerRow(
                    peer = peer,
                    state = connection.stateFor(peer),
                    onConnect = { onConnect(peer) },
                )
            }
        }
    }
}

private enum class PeerRowState { IDLE, CONNECTING, CONNECTED, FAILED }

private fun ConnectionUiState.stateFor(peer: DiscoveredPeer): PeerRowState = when (this) {
    is ConnectionUiState.Connecting -> if (peerId == peer.id) PeerRowState.CONNECTING else PeerRowState.IDLE
    is ConnectionUiState.Connected -> if (peerId == peer.id) PeerRowState.CONNECTED else PeerRowState.IDLE
    is ConnectionUiState.Failed -> if (peerId == peer.id) PeerRowState.FAILED else PeerRowState.IDLE
    ConnectionUiState.Idle -> PeerRowState.IDLE
}

@Composable
private fun PeerRow(
    peer: DiscoveredPeer,
    state: PeerRowState,
    onConnect: () -> Unit,
) {
    val shape = RoundedCornerShape(12.dp)
    val active = state == PeerRowState.CONNECTED || state == PeerRowState.CONNECTING
    Row(
        modifier = Modifier
            .fillMaxWidth()
            .background(if (active) MouserColors.Accent else MouserColors.ChipIdle, shape)
            .border(
                width = 1.dp,
                color = if (active) Color.White.copy(alpha = 0.25f) else Color.Transparent,
                shape = shape,
            )
            .clickable(enabled = state != PeerRowState.CONNECTING, onClick = onConnect)
            .padding(horizontal = 14.dp, vertical = 12.dp)
            .testTag("peer.row.${peer.id}"),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        StatusDot(state = state)
        Column(modifier = Modifier.padding(start = 10.dp).weight(1f)) {
            Text(
                text = peer.name,
                color = if (active) Color.White else MouserColors.OnSurface,
                fontSize = 15.sp,
                fontWeight = FontWeight.SemiBold,
            )
            Text(
                text = "${peer.hostAddress}:${peer.port}",
                color = if (active) Color.White.copy(alpha = 0.8f) else MouserColors.OnSurfaceDim,
                fontSize = 12.sp,
            )
        }
        Text(
            text = state.label(),
            color = if (active) Color.White else MouserColors.OnSurfaceDim,
            fontSize = 12.sp,
            modifier = Modifier.testTag("peer.row.${peer.id}.status"),
        )
    }
}

@Composable
private fun StatusDot(state: PeerRowState) {
    val color = when (state) {
        PeerRowState.CONNECTED -> MouserColors.Live
        PeerRowState.CONNECTING -> Color.White
        PeerRowState.FAILED -> Color(0xFFFF453A) // iOS system red (dark)
        PeerRowState.IDLE -> MouserColors.OnSurfaceFaint
    }
    Box(
        modifier = Modifier
            .size(8.dp)
            .background(color, CircleShape),
    )
}

private fun PeerRowState.label(): String = when (this) {
    PeerRowState.CONNECTED -> "Connected"
    PeerRowState.CONNECTING -> "Connecting…"
    PeerRowState.FAILED -> "Failed — retry"
    PeerRowState.IDLE -> "Tap to connect"
}
