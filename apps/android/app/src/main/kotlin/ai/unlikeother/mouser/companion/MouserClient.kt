package ai.unlikeother.mouser.companion

import android.content.Context
import android.os.Build
import android.provider.Settings
import android.util.Log
import uniffi.mouser_ffi.MobileClient
import uniffi.mouser_ffi.MobileException
import kotlin.math.roundToInt

/**
 * Thin Kotlin façade over the uniffi-generated [MobileClient] (the shared
 * `mouser-ffi` Rust bridge). It owns the native client and translates the
 * companion's recognised [TouchpadEvent]s into the source/controller FFI calls
 * (`sendPointerMoved` / `sendButton` / `sendScroll` / `sendKey`) — the Android
 * parity of the iOS `MobileClient` wiring.
 *
 * Why a wrapper and not the generated object directly:
 *  - The FFI's `sendPointerMoved` reports an **absolute** cursor sample (the Rust
 *    engine forwards the *motion* between successive samples); the [GestureEngine]
 *    emits **relative** deltas. We integrate the deltas into a virtual position
 *    here so the engine sees a coherent motion stream (§7.6).
 *  - It maps the gesture vocabulary (left/right click, click-drag, scroll) onto
 *    the button/scroll wire calls, and hands typed characters to `sendKey`.
 *  - It keeps the FFI's unsigned types (`UInt`/`UByte`/`UShort`) out of the rest
 *    of the Compose code, and never lets a [MobileException] escape into the UI
 *    thread (every call is best-effort while connected; disconnected events drop).
 *
 * Discovery and pairing are out of scope here (parity with iOS / the FFI doc):
 * [connect] takes an explicit host/port + the peer's base32 `device_id`.
 *
 * Identity persistence (parity with the iOS `MouserClient` + `IdentityStore`): build
 * the production client with [create], which restores the persisted seed (stable
 * `device_id` across launches) or generates + persists a fresh one on first run.
 *
 * @param client the native bridge (injected so tests can pass a fresh in-memory one).
 * @param deviceName this controller's display name, sent to the desktop on [connect]
 *   so its pairing prompt can identify us.
 */
class MouserClient(
    private val client: MobileClient = MobileClient(),
    private val deviceName: String = DEFAULT_DEVICE_NAME,
) {
    /** Virtual absolute cursor the relative gesture deltas integrate into. Starts
     *  centred in the engine's large virtual span so ordinary motion never clamps. */
    private var cursorX: Int = VIRTUAL_CENTER
    private var cursorY: Int = VIRTUAL_CENTER

    /** This device's own base32 `device_id` (what the peer must pin against). */
    val deviceId: String get() = runCatching { client.deviceId() }.getOrDefault("")

    val isConnected: Boolean get() = runCatching { client.isConnected() }.getOrDefault(false)

    /**
     * Connect to a peer engine at `host:port`, pinning `peerDeviceIdBase32`, and
     * start forwarding input as the source. Returns the failure (if any) rather
     * than throwing, so the UI can surface it without a crash.
     */
    fun connect(host: String, port: Int, peerDeviceIdBase32: String): Result<Unit> = runCatching {
        cursorX = VIRTUAL_CENTER
        cursorY = VIRTUAL_CENTER
        // Announce our display name so the desktop's pairing prompt can name us (§7.4).
        client.connect(host, port.toUShort(), peerDeviceIdBase32, deviceName)
    }.onFailure { Log.w(TAG, "connect failed", it) }

    /** Tear down the session (idempotent). */
    fun disconnect() {
        runCatching { client.disconnect() }.onFailure { Log.w(TAG, "disconnect failed", it) }
    }

    /**
     * Route one recognised gesture event to the wire. A no-op while disconnected.
     * `Magnify`/`Rotate` have no source-input wire form yet (the FFI exposes only
     * pointer/button/scroll/key), so they are dropped here — they remain local
     * haptic/readout affordances until the protocol carries them.
     */
    fun onEvent(event: TouchpadEvent) {
        if (!isConnected) return
        when (event) {
            is TouchpadEvent.Move -> moveBy(event.dx, event.dy)
            is TouchpadEvent.Scroll -> send { sendScroll(event.dx.roundToInt(), event.dy.roundToInt()) }
            TouchpadEvent.LeftClick -> click(BUTTON_LEFT)
            TouchpadEvent.RightClick -> click(BUTTON_RIGHT)
            TouchpadEvent.DragStart -> send { sendButton(BUTTON_LEFT.toUByte(), true) }
            TouchpadEvent.DragEnd -> send { sendButton(BUTTON_LEFT.toUByte(), false) }
            // Local-only feedback; no source wire representation yet.
            TouchpadEvent.ScrollDetent,
            is TouchpadEvent.Magnify,
            is TouchpadEvent.Rotate -> Unit
        }
    }

    /**
     * Send a typed character as a press+release of its HID usage. Only the subset
     * the capture field produces (ASCII letters/digits/space/return) is mapped;
     * unmapped characters are ignored. Modifier handling (shift for capitals) is a
     * follow-up — the engine lower-cases to the base usage today.
     */
    fun sendCharacter(ch: Char) {
        val usage = hidUsageFor(ch) ?: return
        send {
            sendKey(usage, true, NO_MODS)
            sendKey(usage, false, NO_MODS)
        }
    }

    private fun moveBy(dx: Float, dy: Float) {
        cursorX = (cursorX + dx.roundToInt()).coerceIn(0, VIRTUAL_SPAN)
        cursorY = (cursorY + dy.roundToInt()).coerceIn(0, VIRTUAL_SPAN)
        send { sendPointerMoved(0u, cursorX, cursorY) }
    }

    private fun click(button: Int) = send {
        sendButton(button.toUByte(), true)
        sendButton(button.toUByte(), false)
    }

    /** Run an FFI block, swallowing a [MobileException] so a stray gesture during
     *  teardown can never crash the UI thread. */
    private inline fun send(block: MobileClient.() -> Unit) {
        try {
            client.block()
        } catch (e: MobileException) {
            Log.w(TAG, "send dropped", e)
        }
    }

    companion object {
        private const val TAG = "MouserClient"

        // §7.5 pointer button indices.
        private const val BUTTON_LEFT = 0
        private const val BUTTON_RIGHT = 1

        private val NO_MODS: UShort = 0u

        // Mirrors mouser-ffi VIRTUAL_SPAN (1 << 20); the deltas integrate inside it.
        private const val VIRTUAL_SPAN = 1 shl 20
        private const val VIRTUAL_CENTER = VIRTUAL_SPAN / 2

        /** Fallback display name when no [Context] is available (e.g. in tests). */
        private const val DEFAULT_DEVICE_NAME = "Mouser companion"

        /**
         * Build the production client: restore the persisted identity seed so the
         * `device_id` (and the desktop's trust of this phone) survives an app restart,
         * else generate a fresh identity and persist it. Mirrors the iOS `MouserClient`
         * init + `IdentityStore`. The resolved display name is sent on every [connect].
         */
        fun create(context: Context): MouserClient {
            val store = IdentityStore(context)
            val seed = store.load()
            val native = if (seed != null) {
                MobileClient.fromSeed(seed)
            } else {
                MobileClient().also { runCatching { store.save(it.identitySeed()) } }
            }
            return MouserClient(native, deviceName(context))
        }

        /**
         * This device's user-visible name for the desktop's pairing prompt: the
         * user-set `Settings.Global.DEVICE_NAME` when available, else [Build.MODEL].
         */
        private fun deviceName(context: Context): String {
            val configured = runCatching {
                Settings.Global.getString(context.contentResolver, Settings.Global.DEVICE_NAME)
            }.getOrNull()
            return configured?.takeIf { it.isNotBlank() } ?: Build.MODEL ?: DEFAULT_DEVICE_NAME
        }

        /** Minimal USB HID Usage Page 0x07 map for the capture field's ASCII subset. */
        private fun hidUsageFor(ch: Char): UShort? = when (ch) {
            in 'a'..'z' -> (0x04 + (ch - 'a')).toUShort()
            in 'A'..'Z' -> (0x04 + (ch - 'A')).toUShort()
            in '1'..'9' -> (0x1E + (ch - '1')).toUShort()
            '0' -> 0x27u
            ' ' -> 0x2Cu
            '\n' -> 0x28u // Return/Enter
            else -> null
        }
    }
}
