import Combine
import Foundation

/// A target computer the companion can drive.
///
/// These are real cluster peers (architecture §9) discovered over Bonjour/mDNS by
/// `PeerBrowser` (the `_mouser._udp` service). Each carries the resolved host/port
/// to dial and the peer's base32 `device_id` for the cert-pinned connect (§3). With
/// no peers discovered the UI shows a "searching" state rather than fake devices.
struct Peer: Identifiable, Equatable {
    /// Stable identity: the peer's base32 `device_id` (TXT `id`). Doubles as
    /// `Identifiable.id` so re-resolution of the same service updates in place.
    let id: String
    let name: String
    let kind: Kind
    /// Resolved dialable host (numeric IP or hostname) from the service endpoint.
    let host: String
    /// Resolved dialable interactive UDP port (TXT `iport`, confirmed by the
    /// endpoint resolution).
    let port: UInt16

    /// The peer's base32 `device_id`, used for the cert-pinned connect. Same value
    /// as `id`; named explicitly at the call site for clarity.
    var deviceId: String { id }

    enum Kind {
        case mac
        case windows
        case linux

        /// Map the mDNS TXT `os` key (`macos`/`windows`/`linux`, see
        /// `mouser-engine` discovery) to a chip kind. Unknown → `.linux` (generic).
        init(os: String?) {
            switch os?.lowercased() {
            case "macos": self = .mac
            case "windows": self = .windows
            default: self = .linux
            }
        }

        /// SF Symbol for the selector chip. Generic glyphs — not OS logos.
        var symbolName: String {
            switch self {
            case .mac: return "laptopcomputer"
            case .windows: return "pc"
            case .linux: return "terminal"
            }
        }
    }
}

/// Holds the set of discovered peers and the current selection. Empty until
/// `PeerBrowser` resolves a `_mouser._udp` service; `replace(with:)` is the single
/// seam where real mDNS results land.
@MainActor
final class PeerStore: ObservableObject {
    @Published private(set) var peers: [Peer] = []
    @Published var selectedID: String?

    /// The currently controlled peer, defaulting to the first discovered one.
    var selected: Peer? {
        if let id = selectedID, let match = peers.first(where: { $0.id == id }) {
            return match
        }
        return peers.first
    }

    var isConnected: Bool { selected != nil }

    func select(_ peer: Peer) {
        selectedID = peer.id
    }

    /// Replace the discovered set with the latest browse snapshot (sorted by name
    /// for a stable chip order). Keeps the current selection if that peer is still
    /// present; otherwise clears it so `selected` falls back to the first peer.
    func replace(with discovered: [Peer]) {
        peers = discovered.sorted { $0.name.localizedCaseInsensitiveCompare($1.name) == .orderedAscending }
        if let id = selectedID, !peers.contains(where: { $0.id == id }) {
            selectedID = nil
        }
    }
}
