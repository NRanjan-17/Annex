import CryptoKit
import Foundation
import Network

/// Mac ↔ Mac with no router, over Apple's peer-to-peer Wi-Fi (AWDL, what AirDrop uses).
///
/// The core's sockets can't use that link: macOS only lets a socket onto it through
/// Network.framework's `includePeerToPeer`. So it is relayed. The host advertises
/// `_annexp2p._udp` on peer-to-peer Wi-Fi and passes each flow to the core's QUIC port on
/// 127.0.0.1; the viewer opens a local port that the core connects to like any other address.
/// TLS runs end to end through the relay, so certificate pinning and PIN pairing are unchanged.
///
/// Peer-to-peer Wi-Fi reaches anyone nearby, on any network. So by default a sharing Mac shows
/// itself only to Macs it has paired with (`paired`), and to everyone nearby only for a few
/// minutes when its user asks (`everyone`), like AirDrop's "Everyone for 10 Minutes". A new Mac
/// then pairs with the PIN as usual.
enum PeerToPeer {
    static let serviceType = "_annexp2p._udp"
    /// A relayed flow with no datagrams for this long is over (QUIC sends keep-alives every 2 s).
    static let idleTimeout: TimeInterval = 30

    static func parameters() -> NWParameters {
        let p = NWParameters.udp
        p.includePeerToPeer = true
        return p
    }

    /// Who can see a sharing Mac on peer-to-peer Wi-Fi.
    enum Visibility: Equatable {
        /// Not advertised (no session needs it, or one is running: see `AppModel`).
        case hidden
        /// Only Macs it has paired with recognise it.
        case paired
        /// Every Mac nearby sees its name, for a few minutes.
        case everyone
    }

    /// The TXT keys `annex-discovery` advertises, so both lists read the same.
    static func txt(name: String, fingerprint: String, extend: Bool) -> NWTXTRecord {
        NWTXTRecord(["v": "1", "id": fingerprint, "name": name, "role": "host", "sharing": "1", "extend": extend ? "1" : "0"])
    }

    /// Paired Macs know each other's fingerprint and strangers nearby don't, so a `paired`
    /// advertisement carries no name or fingerprint, only a tag made from the fingerprint and
    /// the quarter hour. A paired viewer recognises it; to anyone else it's a random name that
    /// changes every 15 minutes, so the Mac can't be followed from place to place.
    static let tagPeriod: TimeInterval = 900

    static func epoch(_ date: Date) -> Int { Int(date.timeIntervalSince1970 / tagPeriod) }

    static func tag(fingerprint: String, epoch: Int) -> String {
        let digest = SHA256.hash(data: Data("annex-p2p-v1:\(fingerprint.lowercased()):\(epoch)".utf8))
        return digest.prefix(8).map { String(format: "%02x", $0) }.joined()
    }

    static func pairedTXT(tag: String, extend: Bool) -> NWTXTRecord {
        NWTXTRecord(["v": "1", "t": tag, "role": "host", "sharing": "1", "extend": extend ? "1" : "0"])
    }

    /// What one browse result is to this viewer: a host to list, or nothing. `known` maps the
    /// fingerprints of paired Macs to their names. A tag matches the quarter hour before and
    /// after too, for clocks that differ a little.
    static func host(txt: [String: String], endpoint: NWEndpoint, known: [String: String], now: Date) -> PeerToPeerHost? {
        guard txt["sharing"] != "0" else { return nil }
        let extend = txt["extend"] == "1"
        if let id = txt["id"], id.count == 64 {
            return PeerToPeerHost(fingerprint: id, name: known[id] ?? txt["name"] ?? id, extend: extend, endpoint: endpoint)
        }
        guard let t = txt["t"] else { return nil }
        let e = epoch(now)
        guard let (fp, name) = known.first(where: { fp, _ in
            [e - 1, e, e + 1].contains { tag(fingerprint: fp, epoch: $0) == t }
        }) else { return nil }
        return PeerToPeerHost(fingerprint: fp, name: name, extend: extend, endpoint: endpoint)
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
                case .failed(let e):
                    Annex.log(.debug, "[network] peer-to-peer flow to \(c.endpoint) failed: \(e)")
                    self?.end()
                case .cancelled: self?.end()
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
        Annex.log(.debug, "[network] peer-to-peer flow \(a.endpoint) ended")
        a.cancel()
        b.cancel()
        onEnd?()
    }

    private func pump(_ from: NWConnection, _ to: NWConnection) {
        from.receiveMessage { [weak self] data, _, _, error in
            guard let self, !self.ended else { return }
            if let data, !data.isEmpty {
                self.lastActivity = Date()
                Annex.log(.trace, "[network] peer-to-peer relay \(data.count) bytes to \(to.endpoint)")
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
    private let name: String
    private let fingerprint: String
    private let extend: Bool
    // On `queue`: what's advertised now (the tag's quarter hour for `paired`).
    private var visibility: PeerToPeer.Visibility
    private var advertised: (PeerToPeer.Visibility, Int?)?
    private var refresh: DispatchSourceTimer?

    /// Starts relaying. Tests pass `.hidden`: an ad-hoc signed build has no Local Network
    /// permission, and a refused Bonjour registration stops the listener too.
    init(name: String, fingerprint: String, extend: Bool, corePort: UInt16, visibility: PeerToPeer.Visibility) throws {
        listener = try NWListener(using: PeerToPeer.parameters())
        splices = SpliceSet(queue: queue)
        self.name = name
        self.fingerprint = fingerprint
        self.extend = extend
        self.visibility = visibility
        applyVisibility()
        let core = NWEndpoint.hostPort(host: .ipv4(.loopback), port: NWEndpoint.Port(rawValue: corePort)!)
        listener.newConnectionHandler = { [splices] inbound in
            Annex.log(.info, "[network] peer-to-peer: viewer flow from \(inbound.endpoint)")
            splices.add(Splice(inbound, NWConnection(to: core, using: .udp)))
        }
        listener.stateUpdateHandler = { state in
            switch state {
            case .ready: Annex.log(.info, "[network] peer-to-peer: advertising, relaying to the core on port \(corePort)")
            case .failed(let e), .waiting(let e): Annex.log(.warn, "[network] peer-to-peer: advertising not running: \(e)")
            default: break
            }
        }
        listener.start(queue: queue)
        // The `paired` tag changes every quarter hour.
        let t = DispatchSource.makeTimerSource(queue: queue)
        t.schedule(deadline: .now() + 30, repeating: 30)
        t.setEventHandler { [weak self] in self?.applyVisibility() }
        t.resume()
        refresh = t
    }

    /// Changes who sees this Mac. Flows already relayed keep going.
    func setVisibility(_ v: PeerToPeer.Visibility) {
        queue.async { [weak self] in
            guard let self, self.visibility != v else { return }
            self.visibility = v
            self.applyVisibility()
        }
    }

    /// Registers (or removes) the Bonjour service for `visibility`, if that changed.
    private func applyVisibility() {
        let epoch = visibility == .paired ? PeerToPeer.epoch(Date()) : nil
        if let a = advertised, a.0 == visibility, a.1 == epoch { return }
        advertised = (visibility, epoch)
        switch visibility {
        case .hidden:
            listener.service = nil
        case .everyone:
            listener.service = NWListener.Service(name: name, type: PeerToPeer.serviceType, domain: nil,
                                                  txtRecord: PeerToPeer.txt(name: name, fingerprint: fingerprint, extend: extend))
        case .paired:
            let tag = PeerToPeer.tag(fingerprint: fingerprint, epoch: epoch!)
            listener.service = NWListener.Service(name: "Annex \(tag.prefix(8))", type: PeerToPeer.serviceType, domain: nil,
                                                  txtRecord: PeerToPeer.pairedTXT(tag: tag, extend: extend))
        }
        Annex.log(.info, "[network] peer-to-peer: visible to \(visibility)")
    }

    /// The relay's UDP port once it's listening (for tests; viewers find it through Bonjour).
    var port: UInt16? { listener.port.map(\.rawValue).flatMap { $0 == 0 ? nil : $0 } }

    func stop() {
        // The handlers hold whoever made this; drop them so nothing outlives the session.
        listener.newConnectionHandler = nil
        listener.stateUpdateHandler = nil
        listener.cancel()
        queue.async { [splices, refresh] in
            refresh?.cancel()
            splices.endAll()
        }
    }
}

/// Viewer: finds hosts on peer-to-peer Wi-Fi. Running it turns peer-to-peer Wi-Fi on, which
/// costs some speed on the normal Wi-Fi link, so it runs only while the Connect page is open.
final class PeerToPeerBrowser {
    private let browser = NWBrowser(for: .bonjourWithTXTRecord(type: PeerToPeer.serviceType, domain: nil),
                                    using: PeerToPeer.parameters())

    /// Calls `onChange` on the main queue with every host currently found. `known` gives the
    /// paired Macs (fingerprint → name), which are the only ones that can be recognised unless a
    /// host shows itself to everyone.
    init(known: @escaping () -> [String: String], onChange: @escaping ([PeerToPeerHost]) -> Void) {
        browser.browseResultsChangedHandler = { results, _ in
            let paired = known()
            let now = Date()
            let hosts = results.compactMap { r -> PeerToPeerHost? in
                guard case .bonjour(let txt) = r.metadata else { return nil }
                return PeerToPeer.host(txt: txt.dictionary, endpoint: r.endpoint, known: paired, now: now)
            }
            Annex.log(.debug, "[network] peer-to-peer: \(hosts.count) Mac(s) recognised of \(results.count) found")
            onChange(hosts)
        }
        browser.stateUpdateHandler = { state in
            switch state {
            case .failed(let e), .waiting(let e): Annex.log(.warn, "[network] peer-to-peer: looking for Macs not running: \(e)")
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
            Annex.log(.debug, "[network] peer-to-peer: core flow out to \(host)")
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
                Annex.log(.warn, "[network] peer-to-peer: local relay port failed: \(e)")
                answer(nil)
            default: break
            }
        }
        listener.start(queue: queue)
        DispatchQueue.main.asyncAfter(deadline: .now() + 2) {
            if !answered { Annex.log(.warn, "[network] peer-to-peer: local relay port not ready after 2 s") }
            answer(nil)
        }
    }

    func stop() {
        // The handlers hold whoever made this; drop them so nothing outlives the session.
        listener.newConnectionHandler = nil
        listener.stateUpdateHandler = nil
        listener.cancel()
        queue.async { [splices] in splices.endAll() }
    }
}
