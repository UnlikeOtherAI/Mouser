import Combine
import Foundation

/// A target computer the companion can drive.
///
/// In the real app these are discovered cluster peers (architecture §9),
/// delivered by mDNS browsing + the engine. Until that networking is wired there
/// are **none** — the UI shows a "searching" state rather than inventing fake
/// devices. `PeerStore` is the single seam where real discovery results land.
struct Peer: Identifiable, Equatable {
    let id: String
    let name: String
    let kind: Kind

    enum Kind {
        case mac
        case windows
        case linux

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
/// discovery/the engine lands; publishing into `peers` is the one place real
/// mDNS results will flow once networking exists.
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
}
