import Combine
import Foundation
import Network

/// Native Bonjour/mDNS discovery of computers running `mouserd` on the LAN
/// (architecture §9, spec §4). Browses the `_mouser._udp` service with TXT records
/// via `NWBrowser`, reads each peer's base32 `device_id` (TXT `id`), display name,
/// OS and interactive port (TXT `iport`), then resolves the service endpoint to a
/// concrete host+port so the companion can dial it.
///
/// Browse needs no special entitlement (Info.plist declares `NSBonjourServices`
/// `_mouser._udp` + `NSLocalNetworkUsageDescription`). Discovery is advisory: trust
/// still comes from the §3 cert pin keyed on the `device_id` — never from TXT.
@MainActor
final class PeerBrowser: ObservableObject {
    /// The latest resolved snapshot, published to the UI. Empty while searching.
    @Published private(set) var peers: [Peer] = []

    private var browser: NWBrowser?
    private let queue = DispatchQueue(label: "ai.unlikeother.mouser.peerbrowser")

    /// Endpoints still being resolved (so we don't kick off a second resolve for the
    /// same service while one is in flight).
    private var resolving: Set<NWEndpoint> = []
    /// Per-endpoint short-lived resolver connections, kept alive only until they
    /// report a concrete remote host/port.
    private var resolvers: [NWEndpoint: NWConnection] = [:]
    /// Resolved peers keyed by their service endpoint (one chip per service).
    private var resolved: [NWEndpoint: Peer] = [:]

    /// Begin browsing. Idempotent: a second call while running is a no-op.
    func start() {
        guard browser == nil else { return }

        let params = NWParameters.udp
        params.includePeerToPeer = true
        let descriptor = NWBrowser.Descriptor.bonjourWithTXTRecord(type: "_mouser._udp", domain: nil)
        let browser = NWBrowser(for: descriptor, using: params)
        self.browser = browser

        browser.browseResultsChangedHandler = { [weak self] results, _ in
            // NWBrowser callbacks land on its internal queue; hop to the main actor
            // before touching published state.
            Task { @MainActor in self?.handle(results: results) }
        }
        browser.stateUpdateHandler = { [weak self] state in
            // On a hard failure, tear down so a later start() can rebuild a fresh
            // browser (e.g. after the user grants Local Network permission).
            if case .failed = state {
                Task { @MainActor in self?.stop() }
            }
        }
        browser.start(queue: queue)
    }

    /// Stop browsing and drop all in-flight resolvers and results.
    func stop() {
        browser?.cancel()
        browser = nil
        for connection in resolvers.values { connection.cancel() }
        resolvers.removeAll()
        resolving.removeAll()
        resolved.removeAll()
        peers = []
    }

    // MARK: - Browse reconciliation

    private func handle(results: Set<NWBrowser.Result>) {
        let liveEndpoints = Set(results.map(\.endpoint))

        // Drop peers/resolvers for services that vanished from the LAN.
        for endpoint in Array(resolved.keys) where !liveEndpoints.contains(endpoint) {
            resolved[endpoint] = nil
        }
        for endpoint in resolving.subtracting(liveEndpoints) {
            resolvers[endpoint]?.cancel()
            resolvers[endpoint] = nil
            resolving.remove(endpoint)
        }

        // Resolve any service we don't already have a host/port for.
        for result in results {
            guard !resolving.contains(result.endpoint),
                  resolved[result.endpoint] == nil else { continue }
            guard case let .bonjour(txt) = result.metadata else { continue }
            // The base32 device_id is mandatory (the cert-pin key); skip TXT-less
            // or id-less services — they are not dialable peers.
            guard let id = txt["id"], !id.isEmpty else { continue }
            resolve(result.endpoint, id: id, txt: txt)
        }

        publish()
    }

    /// Open a short-lived UDP connection to the `.service` endpoint; on `.ready`,
    /// the resolved numeric remote host/port is on the connection's current path.
    /// Read it, build the `Peer`, then cancel the connection.
    private func resolve(_ endpoint: NWEndpoint, id: String, txt: NWTXTRecord) {
        resolving.insert(endpoint)
        let name = txt["name"].flatMap { $0.isEmpty ? nil : $0 } ?? defaultName(for: endpoint)
        let kind = Peer.Kind(os: txt["os"])

        let connection = NWConnection(to: endpoint, using: .udp)
        resolvers[endpoint] = connection
        connection.stateUpdateHandler = { [weak self] state in
            switch state {
            case .ready:
                let resolvedHostPort = connection.currentPath?.remoteEndpoint
                Task { @MainActor in
                    self?.finishResolve(endpoint, id: id, name: name, kind: kind, resolved: resolvedHostPort)
                }
            case .failed, .cancelled:
                Task { @MainActor in self?.abandonResolve(endpoint) }
            default:
                break
            }
        }
        connection.start(queue: queue)
    }

    private func finishResolve(
        _ endpoint: NWEndpoint,
        id: String,
        name: String,
        kind: Peer.Kind,
        resolved resolvedEndpoint: NWEndpoint?
    ) {
        // The resolver has served its purpose; close it regardless of outcome.
        resolvers[endpoint]?.cancel()
        resolvers[endpoint] = nil
        resolving.remove(endpoint)

        guard case let .hostPort(host, port) = resolvedEndpoint else { return }
        let peer = Peer(
            id: id,
            name: name,
            kind: kind,
            host: hostString(host),
            port: port.rawValue
        )
        resolved[endpoint] = peer
        publish()
    }

    private func abandonResolve(_ endpoint: NWEndpoint) {
        resolvers[endpoint]?.cancel()
        resolvers[endpoint] = nil
        resolving.remove(endpoint)
    }

    private func publish() {
        peers = Array(resolved.values)
            .sorted { $0.name.localizedCaseInsensitiveCompare($1.name) == .orderedAscending }
    }

    // MARK: - Helpers

    /// A numeric/string form of the resolved host for `NWConnection(host:port:)`.
    /// IPv6 link-local addresses keep their `%zone` scope so the dial reaches the
    /// right interface.
    private func hostString(_ host: NWEndpoint.Host) -> String {
        switch host {
        case let .ipv4(address):
            return "\(address)"
        case let .ipv6(address):
            return "\(address)"
        case let .name(name, _):
            return name
        @unknown default:
            return "\(host)"
        }
    }

    /// Fallback display name when the service omits a TXT `name`: the Bonjour
    /// instance name from the service endpoint.
    private func defaultName(for endpoint: NWEndpoint) -> String {
        if case let .service(serviceName, _, _, _) = endpoint, !serviceName.isEmpty {
            return serviceName
        }
        return "Computer"
    }
}
