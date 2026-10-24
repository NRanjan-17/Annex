import Foundation
import Network

/// Mac ↔ Mac with no router, over Apple's peer-to-peer Wi-Fi (AWDL, what AirDrop uses).
///
/// The core's sockets can't use that link: macOS only lets a socket onto it through
/// Network.framework's `includePeerToPeer`. So it is relayed. The host advertises
/// `_annexp2p._udp` on peer-to-peer Wi-Fi and passes each flow to the core's QUIC port on
/// 127.0.0.1; the viewer opens a local port that the core connects to like any other address.
/// TLS runs end to end through the relay, so certificate pinning and PIN pairing are unchanged.
enum PeerToPeer {
    static let serviceType = "_annexp2p._udp"
    /// A relayed flow with no datagrams for this long is over (QUIC sends keep-alives every 2 s).
    static let idleTimeout: TimeInterval = 30

    static func parameters() -> NWParameters {
        let p = NWParameters.udp
        p.includePeerToPeer = true
        return p
    }

    /// The TXT keys `annex-discovery` advertises, so both lists read the same.
    static func txt(name: String, fingerprint: String, extend: Bool) -> NWTXTRecord {
        NWTXTRecord(["v": "1", "id": fingerprint, "name": name, "role": "host", "sharing": "1", "extend": extend ? "1" : "0"])
    }
}

/// A computer found on peer-to-peer Wi-Fi.
struct PeerToPeerHost: Equatable {
    let fingerprint: String
    let name: String
    let extend: Bool
    let endpoint: NWEndpoint
}

/// Two UDP flows joined: every datagram from one is sent on the other.
private final class Splice {
    let a: NWConnection
    let b: NWConnection
    private(set) var lastActivity = Date()
    private var ended = false
    var onEnd: (() -> Void)?

    init(_ a: NWConnection, _ b: NWConnection) {
        self.a = a
        self.b = b
    }

    func start(on queue: DispatchQueue) {
        for c in [a, b] {
            c.stateUpdateHandler = { [weak self] state in
                switch state {
                case .failed, .cancelled: self?.end()
                default: break
                }
            }
            c.start(queue: queue)
        }
        pump(a, b)
        pump(b, a)
    }

    func end() {
        guard !ended else { return }
        ended = true
        a.cancel()
        b.cancel()
        onEnd?()
    }

    private func pump(_ from: NWConnection, _ to: NWConnection) {
        from.receiveMessage { [weak self] data, _, _, error in
            guard let self, !self.ended else { return }
            if let data, !data.isEmpty {
                self.lastActivity = Date()
                to.send(content: data, completion: .idempotent)
            }
            if error != nil { self.end(); return }
            self.pump(from, to)
        }
    }
}

/// Flows the relay is carrying, dropped once idle.
private final class SpliceSet {
    private var splices: [ObjectIdentifier: Splice] = [:]
    private var timer: DispatchSourceTimer?
    private let queue: DispatchQueue

    init(queue: DispatchQueue) {
        self.queue = queue
        let t = DispatchSource.makeTimerSource(queue: queue)
        t.schedule(deadline: .now() + 5, repeating: 5)
        t.setEventHandler { [weak self] in
            let cutoff = Date().addingTimeInterval(-PeerToPeer.idleTimeout)
            self?.splices.values.filter { $0.lastActivity < cutoff }.forEach { $0.end() }
        }
        t.resume()
        timer = t
    }

    func add(_ s: Splice) {
        let key = ObjectIdentifier(s)
        s.onEnd = { [weak self] in self?.splices.removeValue(forKey: key) }
        splices[key] = s
        s.start(on: queue)
    }

    func endAll() {
        timer?.cancel()
        timer = nil
        splices.values.forEach { $0.end() }
        splices.removeAll()
    }
}

/// Host: advertises this Mac on peer-to-peer Wi-Fi while it shares, relaying to the core.
final class PeerToPeerAdvertiser {
    private let queue = DispatchQueue(label: "annex.p2p.host")
    private let listener: NWListener
    private let splices: SpliceSet

    /// `advertise: false` relays without registering with Bonjour (tests: an ad-hoc signed build
    /// has no Local Network permission, and a refused registration stops the listener too).
    init(name: String, fingerprint: String, extend: Bool, corePort: UInt16, advertise: Bool = true) throws {
        listener = try NWListener(using: PeerToPeer.parameters())
        splices = SpliceSet(queue: queue)
        if advertise {
            listener.service = NWListener.Service(name: name, type: PeerToPeer.serviceType, domain: nil,
                                                  txtRecord: PeerToPeer.txt(name: name, fingerprint: fingerprint, extend: extend))
        }
        let core = NWEndpoint.hostPort(host: .ipv4(.loopback), port: NWEndpoint.Port(rawValue: corePort)!)
        listener.newConnectionHandler = { [splices] inbound in
            Annex.log("p2p: viewer flow from \(inbound.endpoint)")
            splices.add(Splice(inbound, NWConnection(to: core, using: .udp)))
        }
        listener.stateUpdateHandler = { state in
            switch state {
            case .ready: Annex.log("p2p: advertising on peer-to-peer Wi-Fi")
            case .failed(let e), .waiting(let e): Annex.log("p2p: advertising not running: \(e)", error: true)
            default: break
            }
        }
        listener.start(queue: queue)
    }

    /// The relay's UDP port once it's listening (for tests; viewers find it through Bonjour).
    var port: UInt16? { listener.port.map(\.rawValue).flatMap { $0 == 0 ? nil : $0 } }

    func stop() {
        // The handlers hold whoever made this; drop them so nothing outlives the session.
        listener.newConnectionHandler = nil
        listener.stateUpdateHandler = nil
        listener.cancel()
        queue.async { [splices] in splices.endAll() }
    }
}

/// Viewer: finds hosts on peer-to-peer Wi-Fi. Running it turns peer-to-peer Wi-Fi on, which
/// costs some speed on the normal Wi-Fi link, so it runs only while the Connect page is open.
final class PeerToPeerBrowser {
    private let browser = NWBrowser(for: .bonjourWithTXTRecord(type: PeerToPeer.serviceType, domain: nil),
                                    using: PeerToPeer.parameters())

    /// Called on the main queue with every host currently found.
    init(onChange: @escaping ([PeerToPeerHost]) -> Void) {
        browser.browseResultsChangedHandler = { results, _ in
            let hosts = results.compactMap { r -> PeerToPeerHost? in
                guard case .bonjour(let txt) = r.metadata, let id = txt["id"], id.count == 64,
                      txt["sharing"] != "0" else { return nil }
                return PeerToPeerHost(fingerprint: id, name: txt["name"] ?? id, extend: txt["extend"] == "1", endpoint: r.endpoint)
            }
            onChange(hosts)
        }
        browser.stateUpdateHandler = { state in
            switch state {
            case .failed(let e), .waiting(let e): Annex.log("p2p: looking for Macs not running: \(e)", error: true)
            default: break
            }
        }
        browser.start(queue: .main)
    }

    func stop() { browser.cancel() }
}

/// Viewer: a local port the core connects to, carried to one host over peer-to-peer Wi-Fi.
final class PeerToPeerTunnel {
    private let queue = DispatchQueue(label: "annex.p2p.viewer")
    private let listener: NWListener
    private let splices: SpliceSet

    /// Calls `ready` on the main queue with the local address to give the core, or nil.
    init(to host: NWEndpoint, ready: @escaping (String?) -> Void) throws {
        let local = NWParameters.udp
        // Only the core on this Mac may use the tunnel.
        local.acceptLocalOnly = true
        listener = try NWListener(using: local)
        splices = SpliceSet(queue: queue)
        listener.newConnectionHandler = { [splices] fromCore in
            splices.add(Splice(fromCore, NWConnection(to: host, using: PeerToPeer.parameters())))
        }
        var answered = false
        let answer = { (address: String?) in
            DispatchQueue.main.async {
                guard !answered else { return }
                answered = true
                ready(address)
            }
        }
        listener.stateUpdateHandler = { [weak listener] state in
            switch state {
            case .ready:
                answer(listener?.port.map { "127.0.0.1:\($0.rawValue)" })
            case .failed(let e):
                Annex.log("p2p: local relay port failed: \(e)", error: true)
                answer(nil)
            default: break
            }
        }
        listener.start(queue: queue)
        DispatchQueue.main.asyncAfter(deadline: .now() + 2) { answer(nil) }
    }

    func stop() {
        // The handlers hold whoever made this; drop them so nothing outlives the session.
        listener.newConnectionHandler = nil
        listener.stateUpdateHandler = nil
        listener.cancel()
        queue.async { [splices] in splices.endAll() }
    }
}
