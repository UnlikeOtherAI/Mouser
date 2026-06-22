package ai.unlikeother.mouser.companion

import android.content.Context

/**
 * Remembers the base32 `device_id` of the last computer we connected to, so the next
 * launch can auto-reconnect to it once it reappears on the LAN. Android parity of the
 * iOS `UserDefaults` key `"mouser.lastDevice"` in `CompanionView`.
 *
 * This is a public `device_id` (the cert-pin key, not a secret), so a plain
 * [android.content.SharedPreferences] is appropriate — unlike the identity *seed*,
 * which lives in the encrypted [IdentityStore].
 */
class LastDeviceStore(context: Context) {

    private val prefs =
        context.applicationContext.getSharedPreferences(PREFS_NAME, Context.MODE_PRIVATE)

    /** The last connected peer's `device_id`, or `null` if none recorded. */
    val lastDeviceId: String?
        get() = prefs.getString(KEY_LAST_DEVICE, null)?.takeIf { it.isNotEmpty() }

    /** Record the peer we just connected to so the next launch can reconnect to it. */
    fun remember(deviceId: String) {
        prefs.edit().putString(KEY_LAST_DEVICE, deviceId).apply()
    }

    private companion object {
        const val PREFS_NAME = "mouser.prefs"
        const val KEY_LAST_DEVICE = "mouser.lastDevice"
    }
}
