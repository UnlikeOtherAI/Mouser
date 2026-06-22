package ai.unlikeother.mouser.companion

import android.util.Log
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import androidx.lifecycle.DefaultLifecycleObserver
import androidx.lifecycle.LifecycleOwner
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.launch
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow

/**
 * App-lifecycle / connection state holder for the companion (audit R2 HIGH:
 * "no lifecycle handling").
 *
 * Today the companion is a local-only mock — no FFI, no socket (audit C2-2) — so
 * there is nothing to *stream* yet. This holder is the single place those
 * reactions are wired so that when the `mouser-ffi` surface lands (connect/pair,
 * `send_pointer_*`, `request_ownership`) only the bodies below change, not every
 * call-site:
 *
 *  - **on stop/pause** → stop streaming gestures, stop the inertia/frame loop,
 *    and (once networking exists) **yield ownership** so a backgrounded phone
 *    never holds the cluster's active cursor (architecture §9, spec §7.1
 *    `Goodbye{Sleep}`).
 *  - **on resume** → trigger a reconnect (the engine's supervised-reconnect
 *    entry point) and resume streaming.
 *
 * It is a single instance owned by [MainActivity] (so it survives the Compose
 * tree and matches the process lifecycle), observed both by an activity-level
 * [DefaultLifecycleObserver] and — for the frame loop specifically — by a
 * Compose `LifecycleEventEffect` in [CompanionScreen].
 */
class CompanionSession(
    private val mouser: MouserClient? = null,
    private val discovery: PeerDiscovery? = null,
) {

    /** Off-main-thread scope for the QUIC dial (connect blocks on the FFI runtime). */
    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.IO)

    /**
     * True while the app is in the foreground (resumed). The touchpad's inertia
     * frame loop and any future gesture streaming key off this so they genuinely
     * stop in the background instead of spinning a coroutine while hidden.
     */
    var isForeground by mutableStateOf(true)
        private set

    /**
     * True once a control connection to the active peer is up. Reflects the
     * [MouserClient]'s native session when one is wired; the UI reads it to show a
     * "reconnecting…" affordance after a resume.
     */
    var isConnected by mutableStateOf(false)
        private set

    /** Live `_mouser._udp` peers (empty when no discovery is wired, e.g. tests). */
    val peers: StateFlow<List<DiscoveredPeer>> =
        discovery?.peers ?: MutableStateFlow<List<DiscoveredPeer>>(emptyList()).asStateFlow()

    private val _connection = MutableStateFlow<ConnectionUiState>(ConnectionUiState.Idle)
    /** The tap-to-connect phase the [PeerSelector] highlights. */
    val connection: StateFlow<ConnectionUiState> = _connection.asStateFlow()

    /**
     * Dial a discovered peer off the main thread (the FFI `connect` blocks on its
     * tokio runtime). Reflects the phase in [connection] / [isConnected]. A
     * no-op-safe disconnect of any prior session runs first so tapping a second
     * peer cleanly switches.
     */
    fun connect(peer: DiscoveredPeer) {
        val client = mouser ?: return
        _connection.value = ConnectionUiState.Connecting(peer.id)
        scope.launch {
            client.disconnect()
            val result = client.connect(peer.hostAddress, peer.port, peer.id)
            val up = result.isSuccess && client.isConnected
            isConnected = up
            _connection.value =
                if (up) ConnectionUiState.Connected(peer.id) else ConnectionUiState.Failed(peer.id)
            Log.d(TAG, "connect ${peer.id} → $up")
        }
    }

    /**
     * Resume hook. Called from the activity's `onResume` and mirrored by the
     * Compose lifecycle effect. Idempotent.
     *
     * Extension point: kick the reconnect supervisor and re-arm gesture
     * streaming here. For now it only flips [isForeground] so the frame loop
     * restarts.
     */
    fun onResume() {
        isForeground = true
        // Reflect the native session state on resume. A supervised reconnect (walking
        // the backoff and re-issuing request_ownership) is a follow-up; connect is
        // explicit (host/port) today, mirroring iOS / the mouser-ffi scope note.
        isConnected = mouser?.isConnected ?: false
        // Resume LAN discovery so the peer list repopulates while in the foreground.
        discovery?.start()
        Log.d(TAG, "resume → isConnected=$isConnected")
    }

    /**
     * Pause/stop hook. Called from the activity's `onStop` and mirrored by the
     * Compose lifecycle effect. Idempotent.
     *
     * Extension point: stop the per-peer send loop and **yield ownership** so a
     * backgrounded device doesn't strand the cursor. For now it flips
     * [isForeground] so the inertia/frame loop stops.
     */
    fun onStop() {
        isForeground = false
        // Tear down the native session so a backgrounded device doesn't strand the
        // cluster's active cursor: disconnect closes the QUIC connection (the engine's
        // Drop sends a graceful CONNECTION_CLOSE), relinquishing ownership (spec §7.1).
        // A lighter-weight Goodbye{Sleep}+resume-reconnect path is a follow-up.
        mouser?.disconnect()
        isConnected = false
        _connection.value = ConnectionUiState.Idle
        // Stop browsing + release the multicast lock while backgrounded (battery).
        discovery?.stop()
        Log.d(TAG, "stop → stop frame loop + disconnect (yield ownership)")
    }

    /**
     * Activity-level observer bridging the platform [LifecycleOwner] to the
     * [onResume]/[onStop] hooks above. Installed in [MainActivity.onCreate]; this
     * is the `DefaultLifecycleObserver` the audit asks for.
     */
    fun asLifecycleObserver(): DefaultLifecycleObserver = object : DefaultLifecycleObserver {
        override fun onResume(owner: LifecycleOwner) = this@CompanionSession.onResume()
        override fun onStop(owner: LifecycleOwner) = this@CompanionSession.onStop()
    }

    private companion object {
        const val TAG = "CompanionSession"
    }
}
