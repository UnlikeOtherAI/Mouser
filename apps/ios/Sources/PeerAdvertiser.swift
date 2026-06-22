import Combine
import Foundation
import Network
#if canImport(UIKit)
import UIKit
#endif

/// Publishes this companion's presence on the LAN as a Bonjour/mDNS service so a
/// desktop running `mouserd` can see the phone in its device list and pair/trust it
/// (architecture §9, spec §4 — discovery is advisory; trust still comes from the §3
/// cert pin keyed on the `device_id`, never from TXT).
///
/// The phone is **controller-only**: it dials desktops to drive them, but nothing
/// dials the phone (it runs no receiving engine). So this advert exists purely to be
/// *listed* — it carries `iport=0`, which the desktop treats as "present but not
/// connectable" (`PeerAdvert::from_service_info` parses it, `peer_socket_addr`
/// returns `None`), so the desktop lists the phone but never offers a dial to it.
///
/// Advertising needs the same Local Network grant as browsing (Info.plist declares
/// `NSBonjourServices` `_mouser._udp` + `NSLocalNetworkUsageDescription`, plus the
/// multicast entitlement). This mirrors `PeerBrowser`'s `NWParameters`, dispatch
/// queue, and rebuild-on-`.failed`/`.waiting` handling so the advert survives the
/// first-launch permission prompt and recovers without an app restart.
@MainActor
final class PeerAdvertiser: ObservableObject {
    /// The §4 DNS-SD service type the desktop browses for.
    private static let serviceType = "_mouser._udp"
    /// TXT schema version (§4: `txtvers=1`).
    private static let txtVersion = "1"
    /// Controller-only: not a dial target, so advertise a non-dialable interactive
    /// port. The desktop parses `iport=0` and lists us, but `peer_socket_addr`
    /// returns `None` for it, so no "Connect" dial is attempted against the phone.
    private static let interactivePort: UInt16 = 0

    private var listener: NWListener?
    /// This device's persistent base32 `device_id` (TXT `id`) and display name,
    /// captured by `start(...)` and reused when rebuilding the listener after a
    /// `.failed`/`.waiting` transition (e.g. across the permission grant).
    private var deviceId: String?
    private var deviceName: String = "Mouser companion"
    /// Whether advertising is desired. Set by `start()`, cleared by `stop()`; gates
    /// the rebuild-on-failure retry so a deliberate `stop()` is not undone by a
    /// pending relaunch (mirrors `PeerBrowser.isActive`).
    private var isActive = false
    private let queue = DispatchQueue(label: "ai.unlikeother.mouser.peeradvertiser")

    /// Begin advertising this device (`id` = base32 `device_id`, `name` = display
    /// name). Idempotent: a second call while already running is a no-op, but it also
    /// re-arms a listener that stopped (e.g. call it again on app foreground). The id
    /// is mandatory — without it the desktop can't key trust on us, so we don't
    /// advertise an id-less service.
    func start(id: String, name: String) {
        guard !id.isEmpty else { return }
        isActive = true
        deviceId = id
        deviceName = name.isEmpty ? deviceName : name
        launch()
    }

    /// Build and start a fresh `NWListener` advertising the `_mouser._udp` service.
    /// No-op if one is already running or if we have no id to advertise yet.
    private func launch() {
        guard listener == nil, let id = deviceId else { return }

        let params = NWParameters.udp
        params.includePeerToPeer = true

        let listener: NWListener
        do {
            listener = try NWListener(using: params)
        } catch {
            // Couldn't create the listener (e.g. no network yet) — retry shortly,
            // matching the browser's rebuild-on-failure behaviour.
            scheduleRelaunch()
            return
        }
        self.listener = listener

        listener.service = NWListener.Service(
            // Instance name mirrors the desktop's `PeerAdvert::instance_name`
            // ("<display name> (<short id>)") so colliding names stay unique.
            name: instanceName(name: deviceName, id: id),
            type: Self.serviceType,
            txtRecord: txtRecord(id: id, name: deviceName)
        )

        // The phone runs no receiving engine, so inbound connections are never
        // expected. Accept-and-immediately-cancel keeps the listener valid (a
        // listener with no handler is invalid) without holding a session open.
        listener.newConnectionHandler = { connection in
            connection.cancel()
        }
        listener.stateUpdateHandler = { [weak self] state in
            // NWListener callbacks land on its internal queue; hop to the main actor
            // before touching state, like the browser does.
            Task { @MainActor in self?.handle(state: state) }
        }
        listener.start(queue: queue)
    }

    /// React to listener state. On a hard `.failed` or while `.waiting` — most
    /// commonly the pending or denied **Local Network** permission on first launch —
    /// rebuild a fresh listener shortly, so advertising recovers once the user grants
    /// permission without an app restart (mirrors `PeerBrowser.handle(state:)`).
    private func handle(state: NWListener.State) {
        switch state {
        case .failed, .waiting:
            scheduleRelaunch()
        default:
            break
        }
    }

    /// Drop the dead listener and rebuild after a short delay, as long as
    /// advertising is still wanted (mirrors `PeerBrowser.scheduleRelaunch`).
    private func scheduleRelaunch() {
        guard isActive else { return }
        listener?.cancel()
        listener = nil
        DispatchQueue.main.asyncAfter(deadline: .now() + 2) { [weak self] in
            Task { @MainActor in
                guard let self, self.isActive, self.listener == nil else { return }
                self.launch()
            }
        }
    }

    /// Stop advertising and drop the listener.
    func stop() {
        isActive = false
        listener?.cancel()
        listener = nil
    }

    // MARK: - TXT record

    /// Build the §4 TXT record. Keys mirror `mouser-net`'s `PeerAdvert::txt_map` so
    /// the desktop's `from_service_info` parses us: `txtvers`, `id` (base32
    /// `device_id`, the only key the desktop requires), `name`, `os` ("phone"),
    /// `ver`, `iport` (0 — present-but-not-connectable), `bport`, `caps`, `role`.
    private func txtRecord(id: String, name: String) -> NWTXTRecord {
        var txt = NWTXTRecord()
        txt["txtvers"] = Self.txtVersion
        txt["id"] = id
        txt["name"] = name
        txt["os"] = "phone"
        txt["ver"] = appVersion
        txt["iport"] = String(Self.interactivePort)
        txt["bport"] = "0"
        // Advisory hint only (untrusted): the phone sources input, it doesn't receive.
        txt["caps"] = ""
        txt["role"] = "controller"
        return txt
    }

    // MARK: - Helpers

    /// The DNS-SD instance name, matching `PeerAdvert::instance_name`:
    /// "<display name> (<short id>)" using the first 8 chars of the base32 id.
    private func instanceName(name: String, id: String) -> String {
        let short = String(id.prefix(8))
        return "\(name) (\(short))"
    }

    /// The app's short version string for the advisory `ver` TXT key.
    private var appVersion: String {
        Bundle.main.object(forInfoDictionaryKey: "CFBundleShortVersionString") as? String ?? "0"
    }
}
