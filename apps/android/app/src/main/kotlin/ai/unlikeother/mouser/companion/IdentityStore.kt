package ai.unlikeother.mouser.companion

import android.content.Context
import android.util.Log
import androidx.security.crypto.EncryptedSharedPreferences
import androidx.security.crypto.MasterKey

/**
 * Persists the Rust FFI identity **seed** (32 bytes) in Android secure storage so this
 * device keeps a **stable `device_id`** across launches. Android parity of the iOS
 * `IdentityStore` (which uses the Keychain).
 *
 * Without this the [uniffi.mouser_ffi.MobileClient] generates a fresh identity each
 * launch, so the desktop's trust of the phone (and thus pairing) would not survive an
 * app restart. The seed is private key material, hence [EncryptedSharedPreferences]
 * (AES-256-GCM, wrapped by a Keystore-backed [MasterKey]) rather than a plain
 * `SharedPreferences`.
 *
 * The seed is stored Base64-encoded because [EncryptedSharedPreferences] only persists
 * strings. All reads/writes are best-effort: if the Keystore is unavailable (e.g. a
 * device with a broken keystore provider) we log and fall back to a fresh identity
 * rather than crash — the only cost is re-pairing.
 */
class IdentityStore(context: Context) {

    private val appContext = context.applicationContext

    /** Lazily-built encrypted prefs; null if the Keystore-backed store can't be opened. */
    private val prefs by lazy { openPrefs() }

    /** The persisted seed, or `null` on first run / if it was never saved / unreadable. */
    fun load(): ByteArray? {
        val encoded = runCatching { prefs?.getString(KEY_SEED, null) }
            .onFailure { Log.w(TAG, "identity load failed", it) }
            .getOrNull() ?: return null
        return runCatching { android.util.Base64.decode(encoded, android.util.Base64.NO_WRAP) }
            .getOrNull()
            ?.takeIf { it.isNotEmpty() }
    }

    /** Store (upsert) the seed. Best-effort; a failure leaves the next launch to re-pair. */
    fun save(seed: ByteArray) {
        val encoded = android.util.Base64.encodeToString(seed, android.util.Base64.NO_WRAP)
        runCatching { prefs?.edit()?.putString(KEY_SEED, encoded)?.apply() }
            .onFailure { Log.w(TAG, "identity save failed", it) }
    }

    private fun openPrefs() = runCatching {
        val masterKey = MasterKey.Builder(appContext)
            .setKeyScheme(MasterKey.KeyScheme.AES256_GCM)
            .build()
        EncryptedSharedPreferences.create(
            appContext,
            PREFS_NAME,
            masterKey,
            EncryptedSharedPreferences.PrefKeyEncryptionScheme.AES256_SIV,
            EncryptedSharedPreferences.PrefValueEncryptionScheme.AES256_GCM,
        )
    }.onFailure { Log.w(TAG, "EncryptedSharedPreferences unavailable", it) }.getOrNull()

    private companion object {
        const val TAG = "IdentityStore"
        const val PREFS_NAME = "mouser.identity"
        const val KEY_SEED = "identity-seed"
    }
}
