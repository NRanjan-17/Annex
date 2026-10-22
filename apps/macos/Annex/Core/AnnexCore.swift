import AnnexFFI
import Foundation

/// Thin Swift wrappers over the C API in core/annex-ffi/include/annex.h.
/// Callbacks arrive on Rust threads; wrappers forward them as-is, so handlers must hop to
/// their own queue and must never block waiting on the main thread.

struct AnnexError: LocalizedError {
    let message: String
    var errorDescription: String? { message }

    static func last() -> AnnexError {
        AnnexError(message: String(cString: annex_last_error()))
    }
}

@discardableResult
private func check(_ code: Int32) throws -> Int32 {
    if code < 0 { throw AnnexError.last() }
    return code
}

private func takeString(_ p: UnsafeMutablePointer<CChar>?) -> String {
    guard let p else { return "" }
    defer { annex_string_free(p) }
    return String(cString: p)
}

enum Annex {
    static var version: String { String(cString: annex_version()) }
    static func nowMicros() -> Int64 { annex_now_us() }
    static func displayFingerprint(_ fp: String) -> String { takeString(annex_display_fingerprint(fp)) }
}

/// Holds the Swift closures a C callback dispatches to. Its lifetime is tied to the handle.
final class CallbackBox {
    let onEvent: (CoreEvent) -> Void
    let onFrame: ((UnsafeRawBufferPointer, UInt32, Int64?, Bool) -> Void)?

    init(onEvent: @escaping (CoreEvent) -> Void, onFrame: ((UnsafeRawBufferPointer, UInt32, Int64?, Bool) -> Void)? = nil) {
        self.onEvent = onEvent
        self.onFrame = onFrame
    }

    var context: UnsafeMutableRawPointer { Unmanaged.passUnretained(self).toOpaque() }
}

private let eventTrampoline: AnnexEventCallback = { ctx, json in
    guard let ctx, let json else { return }
    let box = Unmanaged<CallbackBox>.fromOpaque(ctx).takeUnretainedValue()
    box.onEvent(CoreEvent.decode(Data(bytes: json, count: strlen(json))))
}

private let frameTrampoline: AnnexFrameCallback = { ctx, data, len, frameID, ts, keyframe in
    guard let ctx, let data else { return }
    let box = Unmanaged<CallbackBox>.fromOpaque(ctx).takeUnretainedValue()
    box.onFrame?(UnsafeRawBufferPointer(start: data, count: len), frameID, ts == Int64.min ? nil : ts, keyframe)
}

/// Identity + trust store. Create one per process.
final class CoreClient {
    let raw: OpaquePointer

    init(dataDirectory: URL, deviceName: String) throws {
        try FileManager.default.createDirectory(at: dataDirectory, withIntermediateDirectories: true)
        var out: OpaquePointer?
        try check(annex_core_new(dataDirectory.path, deviceName, &out))
        raw = out!
    }

    /// In-memory identity (tests).
    init(ephemeralName: String) throws {
        var out: OpaquePointer?
        try check(annex_core_new_ephemeral(ephemeralName, &out))
        raw = out!
    }

    deinit { annex_core_free(raw) }

    var fingerprint: String { takeString(annex_core_fingerprint(raw)) }
    var deviceName: String { takeString(annex_core_device_name(raw)) }

    var trustedPeers: [TrustedPeer] {
        let json = takeString(annex_core_trusted_peers_json(raw))
        return (try? CoreEvent.decoder.decode([TrustedPeer].self, from: Data(json.utf8))) ?? []
    }

    @discardableResult
    func untrust(_ fingerprint: String) throws -> Bool {
        try check(annex_core_untrust(raw, fingerprint)) == 1
    }

    /// Answers a `pairingPinRequired` event with what the user typed.
    @discardableResult
    func submitPin(requestID: UInt64, pin: String) -> Bool {
        annex_core_submit_pin(raw, requestID, pin) == 1
    }

    /// Cancels a pending pairing on either side.
    @discardableResult
    func cancelRequest(_ requestID: UInt64) -> Bool {
        annex_core_cancel_request(raw, requestID) == 1
    }
}

enum SubmitResult { case sent, noSession, dropped }

final class HostHandle {
    private let core: CoreClient
    private let box: CallbackBox
    private var raw: OpaquePointer?

    init(core: CoreClient, port: UInt16, codec: String = "h264", width: Int, height: Int, fps: Int,
         advertise: Bool, onEvent: @escaping (CoreEvent) -> Void) throws {
        self.core = core
        box = CallbackBox(onEvent: onEvent)
        var out: OpaquePointer?
        try check(annex_host_start(core.raw, port, codec, UInt32(width), UInt32(height), UInt32(fps), advertise,
                                   eventTrampoline, box.context, &out))
        raw = out
    }

    deinit { stop() }

    var port: UInt16 { raw.map { annex_host_port($0) } ?? 0 }

    func setStream(codec: String = "h264", width: Int, height: Int, fps: Int) throws {
        guard let raw else { return }
        try check(annex_host_set_stream(raw, codec, UInt32(width), UInt32(height), UInt32(fps)))
    }

    /// Thread-safe and non-blocking.
    @discardableResult
    func submit(_ data: Data, captureMicros: Int64, keyframe: Bool) -> SubmitResult {
        guard let raw, !data.isEmpty else { return .noSession }
        let r = data.withUnsafeBytes { buf in
            annex_host_submit_frame(raw, buf.bindMemory(to: UInt8.self).baseAddress, buf.count, captureMicros, keyframe)
        }
        switch r {
        case ANNEX_SUBMIT_SENT: return .sent
        case ANNEX_SUBMIT_DROPPED: return .dropped
        default: return .noSession
        }
    }

    /// Ends any session and stops listening. Blocks briefly; call from the main thread.
    func stop() {
        if let raw {
            annex_host_free(raw)
            self.raw = nil
        }
    }
}

final class ViewerHandle {
    private let core: CoreClient
    private let box: CallbackBox
    private var raw: OpaquePointer?

    static let encoder: JSONEncoder = {
        let e = JSONEncoder()
        e.keyEncodingStrategy = .convertToSnakeCase
        return e
    }()

    init(core: CoreClient, address: String, expectedFingerprint: String?, display: DisplayInfo,
         onEvent: @escaping (CoreEvent) -> Void,
         onFrame: @escaping (UnsafeRawBufferPointer, UInt32, Int64?, Bool) -> Void) throws {
        self.core = core
        box = CallbackBox(onEvent: onEvent, onFrame: onFrame)
        let displayJSON = String(decoding: try Self.encoder.encode(display), as: UTF8.self)
        var out: OpaquePointer?
        try check(annex_viewer_connect(core.raw, address, expectedFingerprint, displayJSON,
                                       eventTrampoline, frameTrampoline, box.context, &out))
        raw = out
    }

    deinit { disconnect() }

    func requestKeyframe() {
        if let raw { annex_viewer_request_keyframe(raw) }
    }

    func framePresented(_ frameID: UInt32) {
        if let raw { annex_viewer_frame_presented(raw, frameID) }
    }

    /// Sends Bye and waits for the session to end. Call from the main thread.
    func disconnect() {
        if let raw {
            annex_viewer_free(raw)
            self.raw = nil
        }
    }
}

final class DiscoveryHandle {
    private let core: CoreClient
    private let box: CallbackBox
    private var raw: OpaquePointer?

    init(core: CoreClient, onEvent: @escaping (CoreEvent) -> Void) throws {
        self.core = core
        box = CallbackBox(onEvent: onEvent)
        var out: OpaquePointer?
        try check(annex_discovery_start(core.raw, eventTrampoline, box.context, &out))
        raw = out
    }

    deinit {
        if let raw { annex_discovery_free(raw) }
    }
}
