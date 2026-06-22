package ai.unlikeother.mouser.companion

import android.util.Log
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import androidx.lifecycle.DefaultLifecycleObserver
import androidx.lifecycle.LifecycleOwner

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
class CompanionSession(private val mouser: MouserClient? = null) {

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
