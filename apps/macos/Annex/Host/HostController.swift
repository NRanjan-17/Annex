import CoreVideo
import Foundation

/// Mirror-mode host: advertises on the LAN, and while a viewer is connected captures the
/// chosen display, encodes it and hands access units to the core.
@MainActor
final class HostController {
    struct Status: Equatable {
        var listeningPort: Int?
        var viewerName: String?
        var stream: StreamConfig?
        var hardwareEncoder = false
        var error: String?
    }

    let display: Displays.Info
    let fps = 60
    private let core: CoreClient
    private let preferHardware: Bool
    private var host: HostHandle?
    private let capturer = ScreenCapturer()
    private var encoder: H264Encoder?
    private let lastFrame = LockedBox<CVPixelBuffer>()
    private(set) var status = Status() { didSet { onStatus?(status) } }

    var onStatus: ((Status) -> Void)?
    /// Trust prompts and other events the UI must handle.
    var onEvent: ((CoreEvent) -> Void)?

    init(core: CoreClient, display: Displays.Info, preferHardware: Bool) {
        self.core = core
        self.display = display
        self.preferHardware = preferHardware
    }

    var streamSize: (Int, Int) {
        Displays.streamSize(pixelWidth: display.pixelWidth, pixelHeight: display.pixelHeight)
    }

    func start() throws {
        let (w, h) = streamSize
        let handler: (CoreEvent) -> Void = { [weak self] event in
            DispatchQueue.main.async { self?.handle(event) }
        }
        do {
            host = try HostHandle(core: core, port: 47800, width: w, height: h, fps: fps, advertise: true, onEvent: handler)
        } catch {
            // Default port busy (e.g. the CLI is hosting): take any free port; mDNS advertises it.
            host = try HostHandle(core: core, port: 0, width: w, height: h, fps: fps, advertise: true, onEvent: handler)
        }
        status.listeningPort = Int(host?.port ?? 0)
    }

    func stop() {
        stopCapture()
        host?.stop()
        host = nil
        status = Status()
    }

    private func handle(_ event: CoreEvent) {
        switch event {
        case .sessionStarted(let s):
            status.viewerName = s.peerName
            status.stream = s.stream
            startCapture(stream: s.stream)
        case .sessionEnded:
            status.viewerName = nil
            status.stream = nil
            stopCapture()
        case .keyframeRequested:
            encoder?.requestKeyframe()
            // If the screen is static SCK sends no new frames; re-encode the last one.
            if let pb = lastFrame.value {
                encoder?.encode(pb, captureMicros: Annex.nowMicros())
            }
        default:
            break
        }
        onEvent?(event)
    }

    /// Bumped on every start/stop so a slow encoder setup can't resurrect an ended session.
    private var generation = 0

    private func startCapture(stream: StreamConfig) {
        stopCapture()
        generation += 1
        let gen = generation
        let bitrate = max(4_000_000, stream.width * stream.height * stream.fps / 10)
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

    private func stopCapture() {
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
