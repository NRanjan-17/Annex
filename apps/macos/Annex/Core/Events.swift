import Foundation

/// Mirrors the JSON events emitted by the Rust core (docs/PROTOCOL.md, "FFI events").
struct StreamConfig: Codable, Equatable {
    var codec: String
    var width: Int
    var height: Int
    var fps: Int
}

struct DisplayInfo: Codable, Equatable {
    var widthPx: Int
    var heightPx: Int
    var widthMm: Int = 0
    var heightMm: Int = 0
    var refreshHz: Int = 60
    var scalePercent: Int = 100
    /// Every refresh rate the screen supports at its current size (absent from older versions).
    var refreshRates: [Int]? = nil
}

/// Host side: show `pin` until `pairingFinished` arrives for `requestId`.
struct PairingPin: Decodable, Equatable {
    let requestId: UInt64
    let pin: String
    let peerName: String
    let fingerprintDisplay: String
}

/// Viewer side: ask the user for the PIN the host shows.
struct PairingPinRequired: Decodable, Equatable {
    let requestId: UInt64
    let peerName: String
    let fingerprintDisplay: String
}

struct PairingFinished: Decodable, Equatable {
    let requestId: UInt64
    let success: Bool
    let peerName: String
}

struct SessionStarted: Decodable, Equatable {
    /// 0 on the viewer side.
    let sessionId: UInt64
    /// "mirror" or "extend".
    let mode: String
    let peerName: String
    let fingerprint: String
    let viewerDisplay: DisplayInfo?
    let stream: StreamConfig
    /// "quic" or "tcp".
    let transport: String?
    /// "direct" (a cable between the computers) or "network".
    let link: String?
    /// The viewer's mouse and keyboard control the host in this session.
    let input: Bool?
    /// The host understands pointer movement (protocol 1.2), so Extend can send that instead of
    /// positions and the host's pointer can leave the extended display.
    let pointerDeltas: Bool?

    var connectionDescription: String {
        let t = transport == "quic" ? "QUIC" : transport == "tcp" ? "TCP" : nil
        let l = link == "direct" ? "direct cable" : link == "network" ? "network" : nil
        return [t, l].compactMap { $0 }.joined(separator: " · ")
    }
}

struct LatencyStats: Decodable, Equatable {
    let frames: Int
    let fps: Double
    let p50Ms: Double
    let p95Ms: Double
    let p99Ms: Double
    let maxMs: Double
    let clockRttMs: Double?
}

struct PeerInfo: Decodable, Identifiable, Hashable {
    let instance: String
    let name: String
    let addresses: [String]
    let port: Int
    let fingerprint: String?
    let protoMajor: Int?
    /// Accepting viewers now; false = Annex is open there but not sharing. Older peers omit it.
    let sharing: Bool?
    /// Can add an extra display (Extend); nil if it doesn't say.
    let extend: Bool?

    var id: String { instance }
    var isSharing: Bool { sharing ?? true }
    var canExtend: Bool { extend ?? true }

    /// Every address as `host:port` (IPv6 bracketed), comma-separated. The core races them and
    /// prefers a direct cable (link-local address).
    var connectAddress: String? {
        guard !addresses.isEmpty else { return nil }
        return addresses.map { $0.contains(":") ? "[\($0)]:\(port)" : "\($0):\(port)" }.joined(separator: ",")
    }
}

struct TrustedPeer: Decodable, Identifiable, Hashable {
    let fingerprint: String
    let name: String
    let addedAt: UInt64
    let lastSeen: UInt64
    /// Host side: this device may control this Mac's mouse and keyboard.
    var allowInput: Bool = false

    var id: String { fingerprint }

    private enum CodingKeys: String, CodingKey { case fingerprint, name, addedAt, lastSeen, allowInput }

    init(fingerprint: String, name: String, addedAt: UInt64, lastSeen: UInt64, allowInput: Bool = false) {
        (self.fingerprint, self.name, self.addedAt, self.lastSeen, self.allowInput) = (fingerprint, name, addedAt, lastSeen, allowInput)
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        fingerprint = try c.decode(String.self, forKey: .fingerprint)
        name = try c.decode(String.self, forKey: .name)
        addedAt = try c.decode(UInt64.self, forKey: .addedAt)
        lastSeen = try c.decodeIfPresent(UInt64.self, forKey: .lastSeen) ?? 0
        allowInput = try c.decodeIfPresent(Bool.self, forKey: .allowInput) ?? false
    }
}

/// One viewer input event, as the core delivers it to the host (`input` event).
enum InputEvent: Equatable {
    /// Normalized to the stream: 0 = left/top edge, 65535 = right/bottom edge.
    case mouseMove(x: UInt16, y: UInt16)
    /// button: "left", "right" or "middle".
    case mouseButton(button: String, down: Bool)
    /// Pixels; positive dy scrolls up (content moves down), positive dx scrolls left.
    case scroll(dx: Int32, dy: Int32)
    /// USB HID usage (keyboard page).
    case key(usage: UInt16, down: Bool)
    /// The pointer moved by this much (the viewer's raw mouse movement), free to leave the
    /// extended display for this Mac's other screens.
    case mouseDelta(dx: Int32, dy: Int32)
}

extension InputEvent: Decodable {
    private enum CodingKeys: String, CodingKey { case k, x, y, button, down, dx, dy, usage }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        switch try c.decode(String.self, forKey: .k) {
        case "mouse_move": self = .mouseMove(x: try c.decode(UInt16.self, forKey: .x), y: try c.decode(UInt16.self, forKey: .y))
        case "mouse_button": self = .mouseButton(button: try c.decode(String.self, forKey: .button), down: try c.decode(Bool.self, forKey: .down))
        case "scroll": self = .scroll(dx: try c.decode(Int32.self, forKey: .dx), dy: try c.decode(Int32.self, forKey: .dy))
        case "key": self = .key(usage: try c.decode(UInt16.self, forKey: .usage), down: try c.decode(Bool.self, forKey: .down))
        case "mouse_delta": self = .mouseDelta(dx: try c.decode(Int32.self, forKey: .dx), dy: try c.decode(Int32.self, forKey: .dy))
        case let other: throw DecodingError.dataCorruptedError(forKey: .k, in: c, debugDescription: "unknown input kind \(other)")
        }
    }
}

enum CoreEvent: Equatable {
    case listening(port: Int)
    /// Sharing over the internet: the code to show, valid `expiresIn` seconds and once.
    case internetCode(code: String, expiresIn: Int)
    case internetJoined(peerName: String)
    /// The viewer couldn't connect directly; `relayed` if it got a relay address (`limited`: ours).
    case internetRelay(relayed: Bool, limited: Bool)
    /// "used", "expired", "stopped", or what went wrong.
    case internetCodeEnded(reason: String)
    case connecting(address: String)
    case pairingPin(PairingPin)
    case pairingPinRequired(PairingPinRequired)
    case pairingFinished(PairingFinished)
    case sessionStarted(SessionStarted)
    case streamConfig(StreamConfig)
    case keyframeRequested(sessionId: UInt64)
    /// Adaptive bitrate: run this session's encoder at `scalePercent` % of its normal bitrate.
    case bitrateHint(sessionId: UInt64, scalePercent: Int)
    case latencyStats(LatencyStats)
    /// Host: inject this into the session's display.
    case input(sessionId: UInt64, event: InputEvent)
    /// Input was allowed or revoked during a session.
    case inputAllowed(sessionId: UInt64, allowed: Bool)
    /// Host: the viewer wants to control this Mac but isn't allowed yet; ask the user.
    case inputRequested(sessionId: UInt64, peerName: String, fingerprint: String)
    /// Host: the viewer asks for a direct Wi-Fi link.
    case directLinkRequested(sessionId: UInt64)
    /// Join this hotspot for a direct link (credentials arrive only inside the session).
    case directLinkOffer(sessionId: UInt64, ssid: String, passphrase: String)
    case directLinkUnavailable(sessionId: UInt64, reason: String)
    case sessionEnded(sessionId: UInt64, reason: String)
    case error(message: String)
    case peerFound(PeerInfo)
    case peerLost(instance: String)
    case unknown(type: String)

    static let decoder: JSONDecoder = {
        let d = JSONDecoder()
        d.keyDecodingStrategy = .convertFromSnakeCase
        return d
    }()

    private struct Tag: Decodable { let type: String }
    private struct Port: Decodable { let port: Int }
    private struct Code: Decodable { let code: String; let expiresIn: Int }
    private struct Joined: Decodable { let peerName: String }
    private struct Relay: Decodable { let relayed: Bool; let limited: Bool }
    private struct CodeEnded: Decodable { let reason: String }
    private struct Address: Decodable { let address: String }
    private struct Ended: Decodable { let sessionId: UInt64; let reason: String }
    private struct SessionRef: Decodable { let sessionId: UInt64 }
    private struct Hint: Decodable { let sessionId: UInt64; let scalePercent: Int }
    private struct Message: Decodable { let message: String }
    private struct Instance: Decodable { let instance: String }
    private struct Stream: Decodable { let stream: StreamConfig }
    private struct Input: Decodable { let sessionId: UInt64; let event: InputEvent }
    private struct Allowed: Decodable { let sessionId: UInt64; let allowed: Bool }
    private struct Offer: Decodable { let sessionId: UInt64; let ssid: String; let passphrase: String }
    private struct Unavailable: Decodable { let sessionId: UInt64; let reason: String }
    private struct InputAsk: Decodable { let sessionId: UInt64; let peerName: String; let fingerprint: String }

    static func decode(_ data: Data) -> CoreEvent {
        let d = decoder
        guard let tag = try? d.decode(Tag.self, from: data) else { return .unknown(type: "invalid") }
        do {
            switch tag.type {
            case "listening": return .listening(port: try d.decode(Port.self, from: data).port)
            case "internet_code":
                let c = try d.decode(Code.self, from: data)
                return .internetCode(code: c.code, expiresIn: c.expiresIn)
            case "internet_joined": return .internetJoined(peerName: try d.decode(Joined.self, from: data).peerName)
            case "internet_relay":
                let r = try d.decode(Relay.self, from: data)
                return .internetRelay(relayed: r.relayed, limited: r.limited)
            case "internet_code_ended": return .internetCodeEnded(reason: try d.decode(CodeEnded.self, from: data).reason)
            case "connecting": return .connecting(address: try d.decode(Address.self, from: data).address)
            case "pairing_pin": return .pairingPin(try d.decode(PairingPin.self, from: data))
            case "pairing_pin_required": return .pairingPinRequired(try d.decode(PairingPinRequired.self, from: data))
            case "pairing_finished": return .pairingFinished(try d.decode(PairingFinished.self, from: data))
            case "session_started": return .sessionStarted(try d.decode(SessionStarted.self, from: data))
            case "stream_config": return .streamConfig(try d.decode(Stream.self, from: data).stream)
            case "keyframe_requested": return .keyframeRequested(sessionId: try d.decode(SessionRef.self, from: data).sessionId)
            case "bitrate_hint":
                let h = try d.decode(Hint.self, from: data)
                return .bitrateHint(sessionId: h.sessionId, scalePercent: h.scalePercent)
            case "input":
                let i = try d.decode(Input.self, from: data)
                return .input(sessionId: i.sessionId, event: i.event)
            case "input_allowed":
                let a = try d.decode(Allowed.self, from: data)
                return .inputAllowed(sessionId: a.sessionId, allowed: a.allowed)
            case "input_requested":
                let a = try d.decode(InputAsk.self, from: data)
                return .inputRequested(sessionId: a.sessionId, peerName: a.peerName, fingerprint: a.fingerprint)
            case "direct_link_requested":
                return .directLinkRequested(sessionId: try d.decode(SessionRef.self, from: data).sessionId)
            case "direct_link_offer":
                let o = try d.decode(Offer.self, from: data)
                return .directLinkOffer(sessionId: o.sessionId, ssid: o.ssid, passphrase: o.passphrase)
            case "direct_link_unavailable":
                let u = try d.decode(Unavailable.self, from: data)
                return .directLinkUnavailable(sessionId: u.sessionId, reason: u.reason)
            case "latency_stats": return .latencyStats(try d.decode(LatencyStats.self, from: data))
            case "session_ended":
                let e = try d.decode(Ended.self, from: data)
                return .sessionEnded(sessionId: e.sessionId, reason: e.reason)
            case "error": return .error(message: try d.decode(Message.self, from: data).message)
            case "peer_found": return .peerFound(try d.decode(PeerInfo.self, from: data))
            case "peer_lost": return .peerLost(instance: try d.decode(Instance.self, from: data).instance)
            default: return .unknown(type: tag.type)
            }
        } catch {
            return .unknown(type: tag.type)
        }
    }
}
