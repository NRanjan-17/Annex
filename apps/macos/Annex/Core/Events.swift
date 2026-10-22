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
    let peerName: String
    let fingerprint: String
    let viewerDisplay: DisplayInfo?
    let stream: StreamConfig
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

    var id: String { instance }

    /// First address in `host:port` form (IPv6 bracketed).
    var connectAddress: String? {
        guard let a = addresses.first else { return nil }
        return a.contains(":") ? "[\(a)]:\(port)" : "\(a):\(port)"
    }
}

struct TrustedPeer: Decodable, Identifiable, Hashable {
    let fingerprint: String
    let name: String
    let addedAt: UInt64
    let lastSeen: UInt64

    var id: String { fingerprint }
}

enum CoreEvent: Equatable {
    case listening(port: Int)
    case connecting(address: String)
    case pairingPin(PairingPin)
    case pairingPinRequired(PairingPinRequired)
    case pairingFinished(PairingFinished)
    case sessionStarted(SessionStarted)
    case streamConfig(StreamConfig)
    case keyframeRequested
    case latencyStats(LatencyStats)
    case sessionEnded(reason: String)
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
    private struct Address: Decodable { let address: String }
    private struct Reason: Decodable { let reason: String }
    private struct Message: Decodable { let message: String }
    private struct Instance: Decodable { let instance: String }
    private struct Stream: Decodable { let stream: StreamConfig }

    static func decode(_ data: Data) -> CoreEvent {
        let d = decoder
        guard let tag = try? d.decode(Tag.self, from: data) else { return .unknown(type: "invalid") }
        do {
            switch tag.type {
            case "listening": return .listening(port: try d.decode(Port.self, from: data).port)
            case "connecting": return .connecting(address: try d.decode(Address.self, from: data).address)
            case "pairing_pin": return .pairingPin(try d.decode(PairingPin.self, from: data))
            case "pairing_pin_required": return .pairingPinRequired(try d.decode(PairingPinRequired.self, from: data))
            case "pairing_finished": return .pairingFinished(try d.decode(PairingFinished.self, from: data))
            case "session_started": return .sessionStarted(try d.decode(SessionStarted.self, from: data))
            case "stream_config": return .streamConfig(try d.decode(Stream.self, from: data).stream)
            case "keyframe_requested": return .keyframeRequested
            case "latency_stats": return .latencyStats(try d.decode(LatencyStats.self, from: data))
            case "session_ended": return .sessionEnded(reason: try d.decode(Reason.self, from: data).reason)
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
