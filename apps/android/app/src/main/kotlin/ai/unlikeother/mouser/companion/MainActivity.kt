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
 */
class MainActivity : ComponentActivity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        enableEdgeToEdge()
        val haptics = Haptics(this)
        setContent {
            MouserTheme {
                androidx.compose.foundation.layout.Box(
                    modifier = Modifier
                        .fillMaxSize()
                        .background(MouserColors.Background)
                ) {
                    CompanionScreen(haptics = haptics)
                }
            }
        }
    }
}
