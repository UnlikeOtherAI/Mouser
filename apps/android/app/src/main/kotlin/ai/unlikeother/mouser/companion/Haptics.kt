package ai.unlikeother.mouser.companion

import android.content.Context
import android.os.Build
import android.os.VibrationEffect
import android.os.Vibrator
import android.os.VibratorManager

/**
 * Local haptic feedback for trackpad events.
 *
 * Android has no force-touch / 3D-Touch pressure input, so there is no pressure
 * actuation to mirror; we provide crisp transient feedback on discrete actions
 * instead. Uses [VibrationEffect.createPredefined] (EFFECT_CLICK / EFFECT_TICK)
 * on API 29+, falling back to a short one-shot on API 26-28.
 */
class Haptics(context: Context) {

    private val vibrator: Vibrator? = run {
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.S) {
            val mgr = context.getSystemService(Context.VIBRATOR_MANAGER_SERVICE) as? VibratorManager
            mgr?.defaultVibrator
        } else {
            @Suppress("DEPRECATION")
            context.getSystemService(Context.VIBRATOR_SERVICE) as? Vibrator
        }
    }

    private val canVibrate: Boolean get() = vibrator?.hasVibrator() == true

    fun click() = predefinedOrFallback(EffectKind.CLICK, fallbackMs = 12)
    fun rightClick() = predefinedOrFallback(EffectKind.HEAVY_CLICK, fallbackMs = 18)
    fun dragStart() = predefinedOrFallback(EffectKind.HEAVY_CLICK, fallbackMs = 16)
    fun dragEnd() = predefinedOrFallback(EffectKind.TICK, fallbackMs = 8)
    fun scrollDetent() = predefinedOrFallback(EffectKind.TICK, fallbackMs = 6)

    /** Route a [TouchpadEvent] to its corresponding feedback. */
    fun feedback(event: TouchpadEvent) {
        when (event) {
            TouchpadEvent.LeftClick -> click()
            TouchpadEvent.RightClick -> rightClick()
            TouchpadEvent.DragStart -> dragStart()
            TouchpadEvent.DragEnd -> dragEnd()
            TouchpadEvent.ScrollDetent -> scrollDetent()
            else -> Unit // Move/Scroll/Magnify/Rotate are continuous: no per-frame buzz.
        }
    }

    private enum class EffectKind { CLICK, HEAVY_CLICK, TICK }

    private fun predefinedOrFallback(kind: EffectKind, fallbackMs: Long) {
        val v = vibrator ?: return
        if (!canVibrate) return
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
            val id = when (kind) {
                EffectKind.CLICK -> VibrationEffect.EFFECT_CLICK
                EffectKind.HEAVY_CLICK -> VibrationEffect.EFFECT_HEAVY_CLICK
                EffectKind.TICK -> VibrationEffect.EFFECT_TICK
            }
            v.vibrate(VibrationEffect.createPredefined(id))
        } else if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
            v.vibrate(VibrationEffect.createOneShot(fallbackMs, VibrationEffect.DEFAULT_AMPLITUDE))
        } else {
            @Suppress("DEPRECATION")
            v.vibrate(fallbackMs)
        }
    }
}
