package ai.unlikeother.mouser.companion

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
 */
class MouserClient(
    private val client: MobileClient = MobileClient()
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
        client.connect(host, port.toUShort(), peerDeviceIdBase32)
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

    private companion object {
        const val TAG = "MouserClient"

        // §7.5 pointer button indices.
        const val BUTTON_LEFT = 0
        const val BUTTON_RIGHT = 1

        val NO_MODS: UShort = 0u

        // Mirrors mouser-ffi VIRTUAL_SPAN (1 << 20); the deltas integrate inside it.
        const val VIRTUAL_SPAN = 1 shl 20
        const val VIRTUAL_CENTER = VIRTUAL_SPAN / 2

        /** Minimal USB HID Usage Page 0x07 map for the capture field's ASCII subset. */
        fun hidUsageFor(ch: Char): UShort? = when (ch) {
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
