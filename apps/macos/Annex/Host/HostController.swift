import CoreVideo
import Foundation

/// The Mac host: advertises on the LAN and serves up to four viewers. Mirror viewers share one
/// capture of the chosen display; each Extend viewer gets its own virtual display.
@MainActor
final class HostController {
    /// A connected viewer, for per-viewer actions (direct Wi-Fi).
    struct Viewer: Equatable, Identifiable {
        let id: UInt64
        let name: String
        let extend: Bool
    }

    struct Status: Equatable {
        var listeningPort: Int?
        var mirrorViewers: [String] = []
        var extendViewers: [String] = []
        var viewers: [Viewer] = []
        var stream: StreamConfig?
        var hardwareEncoder = false
        /// Viewers currently controlling this Mac.
        var controllingViewers: [String] = []
        var error: String?

        var viewerName: String? { (mirrorViewers + extendViewers).first }
    }

    let display: Displays.Info
    let fps = 60
    private let core: CoreClient
    private let preferHardware: Bool
    /// Settings → Sharing → Extended display scaling (0 = sharpest), for new and running sessions.
    var extendScale = 0 {
        didSet { for s in extendSessions.values { s.setScale(extendScale) } }
    }
    /// Settings → Sound → Share this computer's sound: mirror viewers hear this Mac, for new and
    /// running captures. The core sends it only while a viewer listens.
    var shareSound = false {
        didSet { capturer.setCapturesSound(shareSound) }
    }
    /// Every Extend display has its scaling applied (tests wait for this before changing modes).
    var extendScalingSettled: Bool { extendSessions.values.allSatisfy(\.scaleSettled) }
    private var host: HostHandle?
    private let capturer = ScreenCapturer()
    private var encoder: H264Encoder?
    private let lastFrame = LockedBox<CVPixelBuffer>()
    private var mirrorSessions: [UInt64: String] = [:]
    /// Latest adaptive-bitrate hint per mirror session; the shared encoder follows the lowest.
    private var mirrorScales: [UInt64: Int] = [:]
    /// The mirror encoder's normal bitrate, for the full-size stream (also while the picture is smaller).
    private var mirrorBaseBitrate = 0
    /// The stream mirror sessions started with; nil while there's no mirror capture.
    private var mirrorFullStream: StreamConfig?
    /// What the mirror encoder sends: the full stream, or adaptive bitrate's smaller picture.
    private var mirrorPicture: StreamConfig?
    private var mirrorReduced = false
    private var extendSessions: [UInt64: ExtendSession] = [:]
    private var inputSessions: [UInt64: String] = [:]
    private let injector = InputInjector()
    /// Extend sessions' virtual displays, read on the core's thread for input injection.
    private let extendDisplays = LockedBox<[UInt64: CGDirectDisplayID]>()
    private(set) var status = Status() {
        didSet {
            if let e = status.error, e != oldValue.error { Annex.log(.error, "[host] \(e)") }
            onStatus?(status)
        }
    }

    var onStatus: ((Status) -> Void)?
    /// Pairing and other events the UI must handle.
    var onEvent: ((CoreEvent) -> Void)?

    init(core: CoreClient, display: Displays.Info, preferHardware: Bool) {
        self.core = core
        self.display = display
        self.preferHardware = preferHardware
        capturer.wantsSound = { [weak core] in core?.soundWanted ?? false }
        capturer.onSound = { [weak core] samples, frames, channels, rate in
            core?.pushSound(samples, frames: frames, channels: channels, rate: rate)
        }
    }

    var streamSize: (Int, Int) {
        Displays.streamSize(pixelWidth: display.pixelWidth, pixelHeight: display.pixelHeight,
                            maxW: preferHardware ? 2560 : 1920, maxH: preferHardware ? 1600 : 1200)
    }

    static var extendSupported: Bool { AnnexVirtualDisplay.isSupported() }

    /// Stopping waits for viewers to get the bye and for the connections to close (half a second or
    /// more), so it runs here instead of freezing the app. Serial: a new host waits for the old one
    /// to let go of the port.
    static let teardown = DispatchQueue(label: "annex.host.teardown", qos: .userInitiated)

    func start() throws {
        Self.teardown.sync {}
        let (w, h) = streamSize
        let injector = self.injector
        let extendDisplays = self.extendDisplays
        let mirrorDisplay = display.id
        let handler: (CoreEvent) -> Void = { [weak self] event in
            // Input skips the main queue: it's frequent and latency-sensitive.
            if case .input(let id, let e) = event {
                let display = extendDisplays.value?[id] ?? mirrorDisplay
                Annex.log(.trace, "[input] \(e.logShape) from session \(id) to display \(display)")
                injector.inject(e, display: display)
                return
            }
            DispatchQueue.main.async { self?.handle(event) }
        }
        let extend = Self.extendSupported
        do {
            host = try HostHandle(core: core, port: 47800, width: w, height: h, fps: fps, advertise: true, extend: extend,
                                  onEvent: handler)
        } catch {
            // Default port busy (e.g. the CLI is hosting): take any free port; mDNS advertises it.
            Annex.log(.warn, "[host] port 47800 unavailable (\(error.localizedDescription)); taking any free port")
            host = try HostHandle(core: core, port: 0, width: w, height: h, fps: fps, advertise: true, extend: extend,
                                  onEvent: handler)
        }
        status.listeningPort = Int(host?.port ?? 0)
        Annex.log(.info, "[host] listening on \(status.listeningPort ?? 0), stream up to \(w)×\(h) at \(fps) fps, extend \(extend)")
    }

    func stop() {
        for s in extendSessions.values { s.stop() }
        extendSessions.removeAll()
        extendDisplays.value = [:]
        inputSessions.removeAll()
        injector.releaseAll()
        mirrorSessions.removeAll()
        stopMirrorCapture()
        let host = self.host
        self.host = nil
        Self.teardown.async { host?.stop() }
        status = Status()
    }

    /// Puts what was copied here on the clipboard of each viewer controlling this Mac.
    func sendClipboard(mime: String, data: Data) {
        for (id, name) in inputSessions {
            let sent = host?.sendClipboard(session: id, mime: mime, data: data) == true
            Annex.log(sent ? .debug : .warn, "[clipboard] \(sent ? "sent" : "couldn't send") \(mime), \(data.count) bytes to \(name)")
        }
    }

    /// Sends a file to each viewer controlling this Mac.
    func sendFile(_ url: URL) throws {
        for id in inputSessions.keys { _ = try host?.sendFile(session: id, url: url) }
    }

    /// Pauses or resumes a file being sent to a viewer. False: it isn't being sent.
    func pauseTransfer(session: UInt64, id: UInt64, paused: Bool) -> Bool {
        host?.pauseTransfer(session: session, id: id, paused: paused) == true
    }

    func cancelTransfer(session: UInt64, incoming: Bool, id: UInt64) {
        host?.cancelTransfer(session: session, incoming: incoming, id: id)
    }

    /// A viewer controlling this Mac, by name, if any.
    var controllingViewer: String? { inputSessions.values.sorted().first }

    private func handle(_ event: CoreEvent) {
        if case .sessionStarted(let s) = event, s.input == true {
            inputSessions[s.sessionId] = s.peerName
            warnIfInputBlocked(s.peerName)
        }
        switch event {
        case .sessionStarted(let s) where s.mode == "extend":
            startExtend(s)
        case .inputAllowed(let id, let allowed):
            if allowed, let name = mirrorSessions[id] ?? extendSessions[id]?.viewerName {
                inputSessions[id] = name
                warnIfInputBlocked(name)
            } else if !allowed {
                inputSessions.removeValue(forKey: id)
                Annex.log(.debug, "[input] releasing held keys and buttons: control of session \(id) ended")
                injector.releaseAll()
            }
        case .sessionStarted(let s):
            mirrorSessions[s.sessionId] = s.peerName
            if encoder == nil, ExtendSession.captureEnabled {
                startMirrorCapture(stream: s.stream)
            } else {
                Annex.log(.debug, "[capture] \(s.peerName) joins the running mirror capture")
                // It was told the full size; the picture is smaller for now.
                if mirrorReduced, let p = mirrorPicture {
                    host?.setStream(session: s.sessionId, width: p.width, height: p.height, fps: p.fps)
                }
                requestMirrorKeyframe()
            }
            status.stream = mirrorPicture ?? s.stream
        case .sessionEnded(let id, _):
            if inputSessions.removeValue(forKey: id) != nil {
                Annex.log(.debug, "[input] releasing held keys and buttons: session \(id) ended")
                injector.releaseAll()
            }
            extendDisplays.update { $0?.removeValue(forKey: id) }
            if let ext = extendSessions.removeValue(forKey: id) {
                ext.stop()
            } else if mirrorSessions.removeValue(forKey: id) != nil {
                mirrorScales.removeValue(forKey: id)
                if mirrorSessions.isEmpty {
                    Annex.log(.info, "[capture] last mirror viewer left: stopping capture")
                    stopMirrorCapture()
                    status.stream = nil
                } else {
                    applyMirrorBitrate()
                }
            }
        case .keyframeRequested(let id):
            Annex.log(.debug, "[encode] keyframe asked for by session \(id)")
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
        refreshViewers()
        status.controllingViewers = inputSessions.values.sorted()
        onEvent?(event)
    }

    private func refreshViewers() {
        let mirror = mirrorSessions.map { Viewer(id: $0.key, name: $0.value, extend: false) }
        let extend = extendSessions.map { Viewer(id: $0.key, name: $0.value.viewerName, extend: true) }
        status.viewers = (mirror + extend).sorted { ($0.name, $0.id) < ($1.name, $1.id) }
    }

    /// The viewer may control this Mac, but macOS drops what Annex posts without Accessibility.
    private func warnIfInputBlocked(_ name: String) {
        if !InputInjector.isTrusted {
            Annex.log(.warn, "[permissions] \(name) may control this Mac, but Accessibility is off: macOS ignores its mouse and keyboard")
        }
    }

    /// Asks a viewer (a Windows PC) to start its hotspot for a direct link. False if it's gone.
    func requestDirectLink(session: UInt64) -> Bool {
        host?.requestDirectLink(session: session) ?? false
    }

    /// Shares over the internet with one code; its events arrive through `onEvent`.
    func shareCode(server: String, ownRelay: (url: String, username: String, password: String)?) throws {
        try host?.shareCode(server: server, ownRelay: ownRelay)
    }

    func stopCode() {
        host?.stopCode()
    }

    /// A Mac can't run a hotspot for the direct link; the Windows side does that.
    func refuseDirectLink(session: UInt64) {
        host?.refuseDirectLink(session: session, reason: "This Mac can't host a direct Wi-Fi link. Start the switch from the Windows PC (it runs the hotspot).")
    }

    private func startExtend(_ s: SessionStarted) {
        guard let host else { return Annex.log(.warn, "[display] extend from \(s.peerName) after sharing stopped") }
        Annex.log(.info, "[display] \(s.peerName) extends this Mac: creating a virtual display")
        // Without Screen Recording the extra display would only ever send black, while windows and
        // the pointer move onto it: refuse instead of stranding them on a screen nobody sees.
        if ExtendSession.captureEnabled && !CGPreflightScreenCaptureAccess() {
            status.error = "Allow Annex under Privacy & Security → Screen Recording in System Settings, then try again."
            host.endSession(s.sessionId)
            return
        }
        // The same viewer back before its old session timed out (a dropped connection, or a switch to
        // direct Wi-Fi): that session is dead, and its display has the serial the new one needs.
        for old in extendSessions.values where old.viewerFingerprint == s.fingerprint {
            Annex.log(.info, "[display] \(s.peerName) is back (session \(s.sessionId)): ending its old session \(old.sessionID)")
            extendDisplays.update { $0?.removeValue(forKey: old.sessionID) }
            extendSessions.removeValue(forKey: old.sessionID)?.stop()
            host.endSession(old.sessionID)
        }
        do {
            let session = try ExtendSession(
                sessionID: s.sessionId, host: host, viewer: s.viewerDisplay ?? DisplayInfo(widthPx: 1920, heightPx: 1080),
                viewerFingerprint: s.fingerprint, viewerName: s.peerName, preferHardware: preferHardware,
                scale: extendScale)
            let id = s.sessionId
            // Any capture or encoder failure ends the session, which removes the display again.
            session.onError = { [weak self] message in self?.failExtend(id, message) }
            extendSessions[s.sessionId] = session
            let displayID = session.displayID
            extendDisplays.update { d in
                if d == nil { d = [:] }
                d?[s.sessionId] = displayID
            }
        } catch {
            status.error = error.localizedDescription
            host.endSession(s.sessionId)
        }
    }

    private func failExtend(_ id: UInt64, _ message: String) {
        Annex.log(.info, "[display] extend session \(id) failed; removing its display")
        status.error = message
        extendDisplays.update { $0?.removeValue(forKey: id) }
        extendSessions.removeValue(forKey: id)?.stop()
        status.extendViewers = extendSessions.values.map(\.viewerName).sorted()
        refreshViewers()
        host?.endSession(id)
    }

    /// The bitrate scale the shared mirror encoder runs at: the slowest viewer's.
    private var mirrorScale: Int { mirrorScales.values.min() ?? 100 }

    private func applyMirrorBitrate() {
        guard mirrorBaseBitrate > 0 else { return }
        let scale = mirrorScale
        Annex.log(.debug, "[encode] mirror bitrate \(mirrorBaseBitrate * scale / 100) (\(scale) % of \(mirrorBaseBitrate))")
        encoder?.setBitrate(mirrorBaseBitrate * scale / 100)
        updateMirrorPicture()
    }

    /// The picture for the mirror encoder's current bitrate: the full stream, or (on a slow link)
    /// adaptive bitrate's smaller one, half the size at 30 fps, so text stays readable.
    private func pictureForBitrate(_ full: StreamConfig) -> (reduced: Bool, picture: StreamConfig) {
        let p = Annex.mirrorPicture(effectiveBps: mirrorBaseBitrate * mirrorScale / 100, reduced: mirrorReduced,
                                    width: full.width, height: full.height, fps: full.fps)
        return (p.reduced, StreamConfig(codec: full.codec, width: p.width, height: p.height, fps: p.fps))
    }

    /// Switches the mirror capture to the smaller picture or back when the bitrate crosses the
    /// line. The encoder and capture restart at the new size (the encoder's first frame is a
    /// keyframe); the bitrate stays as it is.
    private func updateMirrorPicture() {
        guard let full = mirrorFullStream else { return }
        let (reduced, picture) = pictureForBitrate(full)
        guard reduced != mirrorReduced else { return }
        mirrorReduced = reduced
        logPicture(picture)
        startMirrorEncoding(picture)
        announceMirrorPicture(picture)
    }

    /// Tells the mirror viewers the picture's size. They keep their window; it's for their stats
    /// and pointer (the decoders follow the keyframe by themselves).
    private func announceMirrorPicture(_ picture: StreamConfig) {
        for id in mirrorSessions.keys {
            host?.setStream(session: id, width: picture.width, height: picture.height, fps: picture.fps)
        }
    }

    private func logPicture(_ picture: StreamConfig) {
        Annex.log(.info, "[encode] sharing: smaller picture \(mirrorReduced ? "on" : "off") "
                  + "bitrate=\(mirrorBaseBitrate * mirrorScale / 100) size=\(picture.width)x\(picture.height) fps=\(picture.fps)")
    }

    private func requestMirrorKeyframe() {
        encoder?.requestKeyframe()
        // If the screen is static SCK sends no new frames; re-encode the last one.
        if let pb = lastFrame.value { encoder?.encode(pb, captureMicros: Annex.nowMicros()) }
    }

    /// Bumped on every start/stop so a slow encoder setup can't resurrect an ended capture.
    private var generation = 0
    /// The last mirror capture failure shown, so a later successful start clears only that.
    private var mirrorCaptureError: String?

    private func startMirrorCapture(stream: StreamConfig) {
        mirrorFullStream = stream
        mirrorBaseBitrate = max(4_000_000, stream.width * stream.height * stream.fps / 10)
        let wasReduced = mirrorReduced
        mirrorReduced = false
        let (reduced, picture) = pictureForBitrate(stream)
        mirrorReduced = reduced
        if reduced { logPicture(picture) }
        startMirrorEncoding(picture)
        if reduced || wasReduced { announceMirrorPicture(picture) }
    }

    /// (Re)starts the mirror encoder and capture at `stream`'s size, keeping the full stream's bitrate.
    private func startMirrorEncoding(_ stream: StreamConfig) {
        stopMirrorEncoding()
        generation += 1
        let gen = generation
        let bitrate = mirrorBaseBitrate
        mirrorPicture = stream
        status.stream = stream
        let preferHardware = self.preferHardware
        Annex.log(.info, "[capture] mirror capture of display \(display.id): \(stream.width)×\(stream.height) at \(stream.fps) fps, "
                  + "H.264 \(bitrate / 1000) kbit/s, \(preferHardware ? "hardware" : "software") encoder preferred")
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
            Annex.log(.debug, "[encode] mirror encoder ready after capture stopped; dropping it")
            enc.invalidate()
            return
        }
        Annex.log(.info, "[encode] mirror encoder ready (\(enc.usingHardware ? "hardware" : "software"))")
        status.hardwareEncoder = enc.usingHardware
        let host = self.host
        if let scale = mirrorScales.values.min(), scale != 100 { enc.setBitrate(mirrorBaseBitrate * scale / 100) }
        enc.onEncoded = { data, key, ts in
            let r = host?.submit(data, captureMicros: ts, keyframe: key)
            Annex.log(.trace, "[encode] mirror frame \(data.count) bytes\(key ? ", keyframe" : ""): \(r.map { "\($0)" } ?? "no host")")
        }
        enc.onFailure = { status in
            Annex.log(.error, "[encode] the mirror encoder keeps failing (OSStatus \(status)); viewers see a frozen picture")
        }
        encoder = enc
        let lastFrame = self.lastFrame
        capturer.onFrame = { [weak enc] pb in
            lastFrame.value = pb
            enc?.encode(pb, captureMicros: Annex.nowMicros())
        }
        capturer.onStop = { [weak self] error in
            DispatchQueue.main.async {
                guard let self else { return }
                self.mirrorCaptureError = "Capture stopped: \(error.localizedDescription)"
                self.status.error = self.mirrorCaptureError
            }
        }
        let capturer = self.capturer
        let id = display.id
        Task { [weak self] in
            do {
                try await capturer.start(displayID: id, width: stream.width, height: stream.height, fps: stream.fps)
                Annex.log(.info, "[capture] mirror capture started")
                // Clear only this capture's own earlier failure, not an unrelated (extend) error.
                guard let self, gen == self.generation, self.status.error == self.mirrorCaptureError else { return }
                self.status.error = nil
            } catch {
                guard let self, gen == self.generation else {
                    return Annex.log(.debug, "[capture] a superseded mirror capture failed: \(error.localizedDescription)")
                }
                let message = "Screen capture failed: \(error.localizedDescription). Allow Annex in System Settings → Privacy & Security → Screen & System Audio Recording."
                self.mirrorCaptureError = message
                self.status.error = message
            }
        }
    }

    private func stopMirrorCapture() {
        stopMirrorEncoding()
        mirrorFullStream = nil
        mirrorPicture = nil
        mirrorReduced = false
    }

    private func stopMirrorEncoding() {
        if encoder != nil { Annex.log(mirrorSessions.isEmpty ? .info : .debug, "[capture] mirror capture stopped") }
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
