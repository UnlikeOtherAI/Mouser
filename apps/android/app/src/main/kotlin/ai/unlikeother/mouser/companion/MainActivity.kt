package ai.unlikeother.mouser.companion

import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.ui.Modifier

/**
 * Single-activity host for the Mouser companion. Handles its own orientation
 * changes (declared in the manifest) so the portrait/landscape layouts swap
 * without recreating, and keeps a single [Haptics] instance for trackpad
 * feedback.
 *
 * Owns the single [CompanionSession] and registers it as a
 * [androidx.lifecycle.DefaultLifecycleObserver] (audit R2 HIGH: app lifecycle),
 * so app stop/resume drive the session's stop-streaming / yield-ownership /
 * reconnect hooks at process scope — independent of the Compose tree.
 */
class MainActivity : ComponentActivity() {

    // Single native bridge for the process. Owned here (not in the Compose tree) so
    // it survives recompositions/orientation changes and its tokio runtime + QUIC
    // connection live for the activity's lifetime.
    private val mouser = MouserClient()
    private val session = CompanionSession(mouser)

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        enableEdgeToEdge()
        // Process-scoped lifecycle hook: stop the frame loop / streaming and yield
        // ownership in the background, reconnect on resume. Mirrored for the frame
        // loop by a LifecycleEventEffect inside CompanionScreen.
        lifecycle.addObserver(session.asLifecycleObserver())
        val haptics = Haptics(this)
        setContent {
            MouserTheme {
                androidx.compose.foundation.layout.Box(
                    modifier = Modifier
                        .fillMaxSize()
                        .background(MouserColors.Background)
                ) {
                    CompanionScreen(haptics = haptics, session = session, mouser = mouser)
                }
            }
        }
    }
}
