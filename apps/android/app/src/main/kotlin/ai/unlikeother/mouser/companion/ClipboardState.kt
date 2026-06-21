package ai.unlikeother.mouser.companion

import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue

/**
 * UI model for the clipboard settings + in-flight transfer, mirroring the
 * `mouser-clipboard` crate so the screens are ready to bind to the engine once
 * the FFI lands (audit C2-2: mobile apps send nothing yet).
 *
 * Every field below maps 1:1 to `mouser_clipboard::settings::ClipboardSettings`
 * (§7.7) or `mouser_clipboard::reassembly::Progress`. Today it is a pure local
 * state holder seeded with mock data — no socket, no `mouser-ffi`. The wiring
 * note on each member says what replaces it.
 */

/** Mirror of `mouser_clipboard::settings::SyncDirection` (§7.7 `direction`). */
enum class SyncDirection {
    /** Offer locally-copied content *and* pull/apply inbound content. */
    BIDIRECTIONAL,

    /** Only offer locally-copied content; never pull/apply inbound offers. */
    SEND_ONLY,

    /** Only pull/apply inbound content; never offer locally-copied content. */
    RECEIVE_ONLY;

    /** Whether this device may **send** (advertise a local offer). */
    val allowsSend: Boolean get() = this == BIDIRECTIONAL || this == SEND_ONLY

    /** Whether this device may **receive** (pull + apply an inbound offer). */
    val allowsReceive: Boolean get() = this == BIDIRECTIONAL || this == RECEIVE_ONLY

    val label: String
        get() = when (this) {
            BIDIRECTIONAL -> "Both ways"
            SEND_ONLY -> "Send only"
            RECEIVE_ONLY -> "Receive only"
        }
}

/**
 * Mirror of `mouser_clipboard::settings::ClipboardSettings` (§7.7). All fields are
 * local, per-device policy enforced on send and on receipt. The defaults match the
 * crate's `Default` (sharing on, all formats on, unlimited size, prefer-native on,
 * bidirectional).
 *
 * @param maxAutoSyncBytes skip eager auto-pull for any representation larger than
 *   this many bytes (`0` = unlimited).
 */
data class ClipboardSettings(
    val sharedClipboard: Boolean = true,
    val syncText: Boolean = true,
    val syncImages: Boolean = true,
    val syncFiles: Boolean = true,
    val maxAutoSyncBytes: Long = 0,
    val preferNativeApple: Boolean = true,
    val direction: SyncDirection = SyncDirection.BIDIRECTIONAL
) {
    /** Master on **and** direction permits sending (`ClipboardSettings::can_offer`). */
    val canOffer: Boolean get() = sharedClipboard && direction.allowsSend

    /** Master on **and** direction permits receiving (`ClipboardSettings::can_receive`). */
    val canReceive: Boolean get() = sharedClipboard && direction.allowsReceive
}

/**
 * Mirror of `mouser_clipboard::reassembly::Progress` plus the peer label the
 * Mac-style wait indicator shows. The engine exposes `received_bytes`/`size`; here
 * we also carry the peer name and transfer direction for the UI.
 */
data class ClipboardTransfer(
    /** The other device in the transfer (origin for a pull, target for a push). */
    val peerName: String,
    /** Whether content is coming **to** this device (pull) or going **from** it. */
    val incoming: Boolean,
    /** Contiguous bytes reassembled/sent so far (`Progress.received_bytes`). */
    val transferredBytes: Long,
    /** Total expected size from the offer (`Progress.size` / `ClipboardEntry.size`). */
    val totalBytes: Long,
    /** What is being moved, for the label (e.g. "image", "12 files", "text"). */
    val kind: String
) {
    /**
     * Fraction in `[0.0, 1.0]` (`Progress::fraction`): a zero-size payload reports
     * complete so an empty clipboard never shows a stuck bar.
     */
    val fraction: Float
        get() = if (totalBytes <= 0L) 1f
        else (transferredBytes.toFloat() / totalBytes.toFloat()).coerceIn(0f, 1f)

    /** Whole-percent for the readout. */
    val percent: Int get() = (fraction * 100f).toInt()

    /** Whether every expected byte has arrived (`Progress::is_complete`). */
    val isComplete: Boolean get() = transferredBytes >= totalBytes
}

/**
 * Local state holder for the clipboard UI (settings + the current transfer).
 *
 * Mock-only: [settings] starts at the crate defaults and edits stay in-process;
 * [transfer] is seeded with a fake in-flight pull so the Mac-style wait indicator
 * is visible/screenshot-verifiable. Once the engine is wired:
 *  - [updateSettings] calls `ClipboardEngine::set_settings`;
 *  - [transfer] is fed from `ClipboardEngine::progress(hash)` (peer + percent).
 */
class ClipboardUiState {

    var settings by mutableStateOf(ClipboardSettings())
        private set

    /** The single in-flight transfer to show in the wait indicator, or `null`. */
    var transfer by mutableStateOf<ClipboardTransfer?>(MOCK_TRANSFER)
        private set

    /** Replace the settings (a control was toggled). FFI: `engine.set_settings`. */
    fun updateSettings(new: ClipboardSettings) {
        settings = new
    }

    /** Set/replace the in-flight transfer (FFI: poll `engine.progress`). */
    fun updateTransfer(new: ClipboardTransfer?) {
        transfer = new
    }

    private companion object {
        /** A fake in-flight image pull so the wait indicator renders without a peer. */
        val MOCK_TRANSFER = ClipboardTransfer(
            peerName = "Mac",
            incoming = true,
            transferredBytes = 1_490_000,
            totalBytes = 2_300_000,
            kind = "image"
        )
    }
}
