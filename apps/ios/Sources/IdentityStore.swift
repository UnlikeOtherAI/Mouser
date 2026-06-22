import Foundation
import Security

/// Persists the Rust FFI identity **seed** (32 bytes) in the iOS Keychain so this device
/// keeps a **stable `device_id`** across launches. Without this the `MobileClient`
/// generates a fresh identity each launch, so the desktop's trust of the phone (and thus
/// pairing) would not survive an app restart. The seed is private key material, hence the
/// Keychain rather than `UserDefaults`.
enum IdentityStore {
    private static let service = "ai.unlikeother.mouser"
    private static let account = "identity-seed"

    /// The persisted seed, or `nil` on first run / if it was never saved.
    static func load() -> Data? {
        let query: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecAttrAccount as String: account,
            kSecReturnData as String: true,
            kSecMatchLimit as String: kSecMatchLimitOne,
        ]
        var item: CFTypeRef?
        let status = SecItemCopyMatching(query as CFDictionary, &item)
        guard status == errSecSuccess, let data = item as? Data, !data.isEmpty else {
            return nil
        }
        return data
    }

    /// Store (upsert) the seed. Delete-then-add keeps it a simple idempotent write.
    static func save(_ seed: Data) {
        let base: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecAttrAccount as String: account,
        ]
        SecItemDelete(base as CFDictionary)
        var add = base
        add[kSecValueData as String] = seed
        add[kSecAttrAccessible as String] = kSecAttrAccessibleAfterFirstUnlock
        SecItemAdd(add as CFDictionary, nil)
    }
}
