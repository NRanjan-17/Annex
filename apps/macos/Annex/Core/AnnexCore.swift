import AnnexFFI
import CoreGraphics
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
    /// Writes the core's log to annex.log in the data folder while on. Off by default.
    static func setLogEnabled(_ on: Bool) { annex_set_log_enabled(on) }
    /// Writes a line to annex.log (while it's on), next to the core's own.
    static func log(_ message: String, error: Bool = false) { annex_log(error ? 2 : 0, message) }
    /// macOS virtual key code for a HID usage from an `input` event.
    static func macKeyCode(hid: UInt16) -> CGKeyCode? {
        let c = annex_key_native_from_hid(hid)
        return c < 0 ? nil : CGKeyCode(c)
    }
}

/// A release worth offering (from the core's update picker).
struct UpdateInfo: Codable, Equatable {
    let version: String
    let prerelease: Bool
    /// Going back from a pre-release to an older normal release.
    let downgrade: Bool
    let notes: String
    let pageUrl: String
    let assetName: String
    let assetUrl: String
    let sumsUrl: String
    let sigUrl: String
}

/// In-app updates: the app downloads, the core decides what to offer and checks signatures.
enum AnnexUpdate {
    static let assetSuffix = "-macos-universal.dmg"

    static var releasesURL: URL { URL(string: String(cString: annex_update_releases_url()))! }

    /// The release to offer from GitHub's releases JSON, or nil if this version is current.
    static func pick(json: String, beta: Bool) throws -> UpdateInfo? {
        var out: UnsafeMutablePointer<CChar>?
        try check(annex_update_pick(json, beta, assetSuffix, &out))
        let text = takeString(out)
        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        return try decoder.decode(UpdateInfo?.self, from: Data(text.utf8))
    }

    /// Throws unless the download matches its release's signed checksums.
    static func verify(sums: URL, sig: URL, file: URL, assetName: String) throws {
        try check(annex_update_verify(sums.path, sig.path, file.path, assetName))
    }
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

    /// Lists this device under Nearby on other computers while Annex is open (as not sharing until
    /// hosting starts). Call at startup and after a rename.
    func setPresence(_ visible: Bool) { annex_core_set_presence(raw, visible) }

    /// Renames this device; returns the name as stored (cleaned and shortened).
    @discardableResult
    func setDeviceName(_ name: String) -> String { takeString(annex_core_set_device_name(raw, name)) }

    var trustedPeers: [TrustedPeer] {
        let json = takeString(annex_core_trusted_peers_json(raw))
        return (try? CoreEvent.decoder.decode([TrustedPeer].self, from: Data(json.utf8))) ?? []
    }

    @discardableResult
    func untrust(_ fingerprint: String) throws -> Bool {
        try check(annex_core_untrust(raw, fingerprint)) == 1
    }

    /// Lets a trusted device control this Mac (or stops it). Returns false if it isn't trusted.
    @discardableResult
    func setAllowInput(_ fingerprint: String, _ allow: Bool) throws -> Bool {
        try check(annex_core_set_allow_input(raw, fingerprint, allow)) == 1
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

    /// `extend`: accept Extend sessions (the app creates a virtual display for each).
    init(core: CoreClient, port: UInt16, codec: String = "h264", width: Int, height: Int, fps: Int,
         advertise: Bool, extend: Bool = false, onEvent: @escaping (CoreEvent) -> Void) throws {
        self.core = core
        box = CallbackBox(onEvent: onEvent)
        var out: OpaquePointer?
        try check(annex_host_start(core.raw, port, codec, UInt32(width), UInt32(height), UInt32(fps), advertise, extend,
                                   eventTrampoline, box.context, &out))
        raw = out
    }

    deinit { stop() }

    var port: UInt16 { raw.map { annex_host_port($0) } ?? 0 }

    func setStream(codec: String = "h264", width: Int, height: Int, fps: Int) throws {
        guard let raw else { return }
        try check(annex_host_set_stream(raw, codec, UInt32(width), UInt32(height), UInt32(fps)))
    }

    /// One access unit for a single (Extend) session. Thread-safe and non-blocking.
    @discardableResult
    func submit(_ data: Data, session: UInt64, captureMicros: Int64, keyframe: Bool) -> SubmitResult {
        guard let raw, !data.isEmpty else { return .noSession }
        let r = data.withUnsafeBytes { buf in
            annex_host_submit_session_frame(raw, session, buf.bindMemory(to: UInt8.self).baseAddress, buf.count,
                                            captureMicros, keyframe)
        }
        switch r {
        case ANNEX_SUBMIT_SENT: return .sent
        case ANNEX_SUBMIT_DROPPED: return .dropped
        default: return .noSession
        }
    }

    func setStream(session: UInt64, width: Int, height: Int, fps: Int) {
        guard let raw else { return }
        annex_host_set_session_stream(raw, session, "h264", UInt32(width), UInt32(height), UInt32(fps))
    }

    /// Answers a direct-link request that this Mac can't meet.
    func refuseDirectLink(session: UInt64, reason: String) {
        if let raw { annex_host_refuse_direct_link(raw, session, reason) }
    }

    /// Asks a viewer to set up a direct Wi-Fi link (a Windows viewer starts its hotspot and answers
    /// with directLinkOffer; others with directLinkUnavailable). Returns false if the session is gone.
    @discardableResult
    func requestDirectLink(session: UInt64) -> Bool {
        guard let raw else { return false }
        return annex_host_request_direct_link(raw, session) == 1
    }

    func endSession(_ session: UInt64) {
        if let raw { annex_host_end_session(raw, session) }
    }

    /// One access unit for every mirror session. Thread-safe and non-blocking.
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

    /// `input`: ask to control the host; `sessionStarted.input` says whether it agreed.
    /// `quality`: 0 best, 1 balanced, 2 fast (bitrate ceiling for the host).
    init(core: CoreClient, address: String, expectedFingerprint: String?, display: DisplayInfo, extend: Bool = false,
         input: Bool = false, quality: Int = 0, onEvent: @escaping (CoreEvent) -> Void,
         onFrame: @escaping (UnsafeRawBufferPointer, UInt32, Int64?, Bool) -> Void) throws {
        self.core = core
        box = CallbackBox(onEvent: onEvent, onFrame: onFrame)
        let displayJSON = String(decoding: try Self.encoder.encode(display), as: UTF8.self)
        var out: OpaquePointer?
        try check(annex_viewer_connect(core.raw, address, expectedFingerprint, displayJSON, extend, input, UInt32(quality),
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

    // Input. Thread-safe and non-blocking; dropped by the core unless the host allows input.

    /// Normalized pointer position over the video: (0, 0) top-left, (1, 1) bottom-right.
    func sendMouseMove(x: Double, y: Double) {
        if let raw { annex_viewer_send_mouse_move(raw, x, y) }
    }

    /// button: 0 left, 1 right, 2 middle.
    func sendMouseDelta(dx: Int32, dy: Int32) {
        if let raw { annex_viewer_send_mouse_delta(raw, dx, dy) }
    }

    func sendMouseButton(_ button: UInt32, down: Bool) {
        if let raw { annex_viewer_send_mouse_button(raw, button, down) }
    }

    func sendScroll(dx: Int32, dy: Int32) {
        if let raw { annex_viewer_send_scroll(raw, dx, dy) }
    }

    /// Asks the host for a direct Wi-Fi link (answered by directLinkOffer / directLinkUnavailable).
    func requestDirectLink() {
        if let raw { annex_viewer_request_direct_link(raw) }
    }

    /// Answers a host's direct-link request that this Mac can't meet.
    func refuseDirectLink(reason: String) {
        if let raw { annex_viewer_refuse_direct_link(raw, reason) }
    }

    func sendKey(macKeyCode: UInt16, down: Bool) {
        if let raw { annex_viewer_send_key(raw, UInt32(macKeyCode), down) }
    }

    /// Sends Bye and waits for the session to end (up to ~300 ms), so AppModel calls it off the main
    /// thread. Call it once, after the window showing this viewer has closed.
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

    /// Re-checks every listed computer and asks the network again (the Nearby refresh button).
    func refresh() {
        if let raw { annex_discovery_refresh(raw) }
    }

    deinit {
        if let raw { annex_discovery_free(raw) }
    }
}
