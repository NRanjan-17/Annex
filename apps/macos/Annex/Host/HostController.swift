import CoreVideo
import Foundation

/// The Mac host: advertises on the LAN and serves up to four viewers. Mirror viewers share one
/// capture of the chosen display; each Extend viewer gets its own virtual display.
@MainActor
final class HostController {
    struct Status: Equatable {
        var listeningPort: Int?
        var mirrorViewers: [String] = []
        var extendViewers: [String] = []
        var stream: StreamConfig?
        var hardwareEncoder = false
        var error: String?

        var viewerName: String? { (mirrorViewers + extendViewers).first }
    }

    let display: Displays.Info
    let fps = 60
    private let core: CoreClient
    private let preferHardware: Bool
    private var host: HostHandle?
    private let capturer = ScreenCapturer()
    private var encoder: H264Encoder?
    private let lastFrame = LockedBox<CVPixelBuffer>()
    private var mirrorSessions: [UInt64: String] = [:]
    /// Latest adaptive-bitrate hint per mirror session; the shared encoder follows the lowest.
    private var mirrorScales: [UInt64: Int] = [:]
    private var mirrorBaseBitrate = 0
    private var extendSessions: [UInt64: ExtendSession] = [:]
    private(set) var status = Status() { didSet { onStatus?(status) } }

    var onStatus: ((Status) -> Void)?
    /// Pairing and other events the UI must handle.
    var onEvent: ((CoreEvent) -> Void)?

    init(core: CoreClient, display: Displays.Info, preferHardware: Bool) {
        self.core = core
        self.display = display
        self.preferHardware = preferHardware
    }

    var streamSize: (Int, Int) {
        Displays.streamSize(pixelWidth: display.pixelWidth, pixelHeight: display.pixelHeight,
                            maxW: preferHardware ? 2560 : 1920, maxH: preferHardware ? 1600 : 1200)
    }

    static var extendSupported: Bool { AnnexVirtualDisplay.isSupported() }

    func start() throws {
        let (w, h) = streamSize
        let handler: (CoreEvent) -> Void = { [weak self] event in
            DispatchQueue.main.async { self?.handle(event) }
        }
        let extend = Self.extendSupported
        do {
            host = try HostHandle(core: core, port: 47800, width: w, height: h, fps: fps, advertise: true, extend: extend,
                                  onEvent: handler)
        } catch {
            // Default port busy (e.g. the CLI is hosting): take any free port; mDNS advertises it.
            host = try HostHandle(core: core, port: 0, width: w, height: h, fps: fps, advertise: true, extend: extend,
                                  onEvent: handler)
        }
        status.listeningPort = Int(host?.port ?? 0)
    }

    func stop() {
        for s in extendSessions.values { s.stop() }
        extendSessions.removeAll()
        mirrorSessions.removeAll()
        stopMirrorCapture()
        host?.stop()
        host = nil
        status = Status()
    }

    private func handle(_ event: CoreEvent) {
        switch event {
        case .sessionStarted(let s) where s.mode == "extend":
            startExtend(s)
        case .sessionStarted(let s):
            mirrorSessions[s.sessionId] = s.peerName
            status.stream = s.stream
            if encoder == nil, ExtendSession.captureEnabled { startMirrorCapture(stream: s.stream) } else { requestMirrorKeyframe() }
        case .sessionEnded(let id, _):
            if let ext = extendSessions.removeValue(forKey: id) {
                ext.stop()
            } else if mirrorSessions.removeValue(forKey: id) != nil {
                mirrorScales.removeValue(forKey: id)
                if mirrorSessions.isEmpty {
                    stopMirrorCapture()
                    status.stream = nil
                } else {
                    applyMirrorBitrate()
                }
            }
        case .keyframeRequested(let id):
            if let ext = extendSessions[id] { ext.requestKeyframe() } else { requestMirrorKeyframe() }
        case .bitrateHint(let id, let scale):
            if let ext = extendSessions[id] {
                ext.setBitrateScale(scale)
            } else if mirrorSessions[id] != nil {
                mirrorScales[id] = scale
                applyMirrorBitrate()
            }
        default:
            break
        }
        status.mirrorViewers = mirrorSessions.values.sorted()
        status.extendViewers = extendSessions.values.map(\.viewerName).sorted()
        onEvent?(event)
    }

    private func startExtend(_ s: SessionStarted) {
        guard let host else { return }
        do {
            let session = try ExtendSession(
                sessionID: s.sessionId, host: host, viewer: s.viewerDisplay ?? DisplayInfo(widthPx: 1920, heightPx: 1080),
                viewerFingerprint: s.fingerprint, viewerName: s.peerName, preferHardware: preferHardware)
            session.onError = { [weak self] message in self?.status.error = message }
            extendSessions[s.sessionId] = session
        } catch {
            status.error = error.localizedDescription
            host.endSession(s.sessionId)
        }
    }

    private func applyMirrorBitrate() {
        guard mirrorBaseBitrate > 0 else { return }
        let scale = mirrorScales.values.min() ?? 100
        encoder?.setBitrate(mirrorBaseBitrate * scale / 100)
    }

    private func requestMirrorKeyframe() {
        encoder?.requestKeyframe()
        // If the screen is static SCK sends no new frames; re-encode the last one.
        if let pb = lastFrame.value { encoder?.encode(pb, captureMicros: Annex.nowMicros()) }
    }

    /// Bumped on every start/stop so a slow encoder setup can't resurrect an ended capture.
    private var generation = 0

    private func startMirrorCapture(stream: StreamConfig) {
        stopMirrorCapture()
        generation += 1
        let gen = generation
        let bitrate = max(4_000_000, stream.width * stream.height * stream.fps / 10)
        mirrorBaseBitrate = bitrate
        let preferHardware = self.preferHardware
        Task.detached(priority: .userInitiated) { [weak self] in
            let result = Result {
                try H264Encoder(width: stream.width, height: stream.height, fps: stream.fps, bitrate: bitrate,
                                preferHardware: preferHardware)
            }
            await self?.encoderReady(result, stream: stream, generation: gen)
        }
    }

    private func encoderReady(_ result: Result<H264Encoder, Error>, stream: StreamConfig, generation gen: Int) {
        let enc: H264Encoder
        switch result {
        case .success(let e): enc = e
        case .failure(let error):
            status.error = error.localizedDescription
            return
        }
        guard gen == generation else {
            enc.invalidate()
            return
        }
        status.hardwareEncoder = enc.usingHardware
        let host = self.host
        if let scale = mirrorScales.values.min(), scale != 100 { enc.setBitrate(mirrorBaseBitrate * scale / 100) }
        enc.onEncoded = { data, key, ts in
            host?.submit(data, captureMicros: ts, keyframe: key)
        }
        encoder = enc
        let lastFrame = self.lastFrame
        capturer.onFrame = { [weak enc] pb in
            lastFrame.value = pb
            enc?.encode(pb, captureMicros: Annex.nowMicros())
        }
        capturer.onStop = { [weak self] error in
            DispatchQueue.main.async { self?.status.error = "Capture stopped: \(error.localizedDescription)" }
        }
        let capturer = self.capturer
        let id = display.id
        Task { [weak self] in
            do {
                try await capturer.start(displayID: id, width: stream.width, height: stream.height, fps: stream.fps)
                self?.status.error = nil
            } catch {
                self?.status.error = "Screen capture failed: \(error.localizedDescription). Allow Annex in System Settings → Privacy & Security → Screen & System Audio Recording."
            }
        }
    }

    private func stopMirrorCapture() {
        generation += 1
        let capturer = self.capturer
        capturer.onFrame = nil
        Task { await capturer.stop() }
        encoder?.invalidate()
        encoder = nil
        lastFrame.value = nil
    }
}

/// A value guarded by a lock, shareable across queues.
final class LockedBox<T>: @unchecked Sendable {
    private let lock = NSLock()
    private var _value: T?

    var value: T? {
        get { lock.lock(); defer { lock.unlock() }; return _value }
        set { lock.lock(); _value = newValue; lock.unlock() }
    }

    /// Atomic read-modify-write.
    @discardableResult
    func update<R>(_ body: (inout T?) -> R) -> R {
        lock.lock()
        defer { lock.unlock() }
        return body(&_value)
    }
}
