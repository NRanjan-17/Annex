import CoreAudio
import CoreMedia
import ScreenCaptureKit

/// Captures one display with ScreenCaptureKit, scaled by the system to the stream size and
/// delivered as NV12 (420v) pixel buffers ready for VideoToolbox; optionally the Mac's own sound
/// too (not Annex's), as interleaved floats.
final class ScreenCapturer: NSObject, SCStreamOutput, SCStreamDelegate {
    // start/stop run off the caller's actor and may overlap, so the stream and a generation
    // (bumped by every start and stop) are guarded by a lock. So are the running stream's
    // configuration and the sound switch, which change while it runs.
    private let lock = NSLock()
    private var stream: SCStream?
    private var config: SCStreamConfiguration?
    private var generation = 0
    private var soundOn = false
    private let queue = DispatchQueue(label: "annex.capture", qos: .userInteractive)
    private let soundQueue = DispatchQueue(label: "annex.capture.sound", qos: .userInteractive)
    /// Interleaving scratch space, used only on `soundQueue`.
    private var interleaved: [Float] = []
    /// The sound format was logged (once per capture, on `soundQueue`).
    private var soundFormatLogged = false

    /// Called on the capture queue for every new frame.
    var onFrame: ((CVPixelBuffer) -> Void)?
    /// Whether anyone wants the sound right now; asked before each chunk is converted.
    var wantsSound: (() -> Bool)?
    /// Called on the sound queue with interleaved float samples: (samples, frames, channels, rate).
    var onSound: ((UnsafePointer<Float>, Int, Int, Int) -> Void)?
    /// Called if the system stops the stream (permission revoked, display removed, ...).
    var onStop: ((Error) -> Void)?

    /// Replaces any running capture. Returns without capturing if `stop()` or another `start` was
    /// called meanwhile, so a stop during start can never leave capture running.
    func start(displayID: CGDirectDisplayID, width: Int, height: Int, fps: Int) async throws {
        let (gen, old) = begin()
        await Self.end(old)
        let content = try await SCShareableContent.excludingDesktopWindows(false, onScreenWindowsOnly: true)
        guard let display = content.displays.first(where: { $0.displayID == displayID }) else {
            Annex.log(.debug, "[capture] display \(displayID) not among \(content.displays.map(\.displayID)) yet")
            throw AnnexError(message: "Display \(displayID) is not available for capture")
        }
        let filter = SCContentFilter(display: display, excludingWindows: [])
        let config = SCStreamConfiguration()
        config.width = width
        config.height = height
        config.minimumFrameInterval = CMTime(value: 1, timescale: CMTimeScale(fps))
        config.pixelFormat = kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange
        config.colorMatrix = CGDisplayStream.yCbCrMatrix_ITU_R_709_2
        // sRGB, as the stream is tagged; left alone, a P3 screen's colours come out dull.
        config.colorSpaceName = CGColorSpace.sRGB
        config.showsCursor = true
        config.queueDepth = 4
        let sound = capturesSound
        config.capturesAudio = sound
        // Annex's own playback (a viewer's microphone, or a session it views) stays out.
        config.excludesCurrentProcessAudio = true
        config.sampleRate = 48000
        config.channelCount = 2

        guard isCurrent(gen) else { return Annex.log(.debug, "[capture] start of display \(displayID) superseded") }
        Annex.log(.debug, "[capture] starting display \(displayID) at \(width)×\(height), \(fps) fps, sound \(sound ? "on" : "off")")
        let stream = SCStream(filter: filter, configuration: config, delegate: self)
        try stream.addStreamOutput(self, type: .screen, sampleHandlerQueue: queue)
        // Always added, so turning sound on later only needs a configuration update.
        try stream.addStreamOutput(self, type: .audio, sampleHandlerQueue: soundQueue)
        soundQueue.async { self.soundFormatLogged = false }
        do {
            try await stream.startCapture()
        } catch {
            if isCurrent(gen) { throw error }
            return
        }
        if !adopt(stream, config: config, generation: gen) {
            Annex.log(.debug, "[capture] display \(displayID) stopped while starting")
            await Self.end(stream)
        }
    }

    /// Capture this Mac's sound with the picture. Applies to a running capture at once.
    var capturesSound: Bool {
        lock.lock()
        defer { lock.unlock() }
        return soundOn
    }

    func setCapturesSound(_ on: Bool) {
        lock.lock()
        soundOn = on
        guard let running = stream, let config, config.capturesAudio != on else { lock.unlock(); return }
        config.capturesAudio = on
        lock.unlock()
        running.updateConfiguration(config) { error in
            if let error {
                Annex.log(.warn, "[capture] couldn't turn sound \(on ? "on" : "off") on the running capture: \(error.localizedDescription)")
            } else {
                Annex.log(.debug, "[capture] sound \(on ? "on" : "off") on the running capture")
            }
        }
    }

    func stop() async {
        let (_, stream) = begin()
        await Self.end(stream)
    }

    /// Stops a stream; failing to is logged, not thrown (it's going away either way).
    private static func end(_ stream: SCStream?) async {
        guard let stream else { return }
        do {
            try await stream.stopCapture()
        } catch {
            Annex.log(.debug, "[capture] stopping a capture: \(error.localizedDescription)")
        }
    }

    // Plain lock/unlock in synchronous helpers: Swift 6.3's type checker crashes on the
    // equivalent `withLock` closures inside the async `start`.

    /// Starts a new generation and takes the current stream (the caller stops it).
    private func begin() -> (Int, SCStream?) {
        lock.lock()
        defer { lock.unlock() }
        generation += 1
        let old = stream
        stream = nil
        config = nil
        return (generation, old)
    }

    /// Keeps `stream` if no `stop()` or newer `start` came since `gen`. If the sound switch
    /// changed while it started, applies that now.
    private func adopt(_ stream: SCStream, config: SCStreamConfiguration, generation gen: Int) -> Bool {
        lock.lock()
        guard gen == generation else { lock.unlock(); return false }
        self.stream = stream
        self.config = config
        let sound = soundOn
        lock.unlock()
        if config.capturesAudio != sound { setCapturesSound(sound) }
        return true
    }

    private func isCurrent(_ gen: Int) -> Bool {
        lock.lock()
        defer { lock.unlock() }
        return gen == generation
    }

    func stream(_ stream: SCStream, didOutputSampleBuffer sampleBuffer: CMSampleBuffer, of type: SCStreamOutputType) {
        if type == .audio { return sound(sampleBuffer) }
        guard type == .screen, sampleBuffer.isValid,
              let attachments = CMSampleBufferGetSampleAttachmentsArray(sampleBuffer, createIfNecessary: false) as? [[SCStreamFrameInfo: Any]],
              let rawStatus = attachments.first?[.status] as? Int,
              SCFrameStatus(rawValue: rawStatus) == .complete,
              let pixelBuffer = sampleBuffer.imageBuffer
        else { return }
        Annex.log(.trace, "[capture] frame \(CVPixelBufferGetWidth(pixelBuffer))×\(CVPixelBufferGetHeight(pixelBuffer))")
        onFrame?(pixelBuffer)
    }

    /// One chunk of this Mac's sound, on the sound queue: float32 (usually one plane per channel)
    /// made interleaved for the core. Skipped while nobody listens. Never logs the samples.
    private func sound(_ sampleBuffer: CMSampleBuffer) {
        guard sampleBuffer.isValid, let onSound, wantsSound?() == true,
              let format = sampleBuffer.formatDescription?.audioStreamBasicDescription
        else { return }
        let channels = Int(format.mChannelsPerFrame)
        let rate = Int(format.mSampleRate)
        let planar = format.mFormatFlags & kAudioFormatFlagIsNonInterleaved != 0
        if !soundFormatLogged {
            soundFormatLogged = true
            Annex.log(.debug, "[capture] sound: \(rate) Hz, \(channels) channel(s), \(format.mBitsPerChannel)-bit "
                      + "\(format.mFormatFlags & kAudioFormatFlagIsFloat != 0 ? "float" : "integer"), \(planar ? "planar" : "interleaved")")
        }
        guard format.mFormatID == kAudioFormatLinearPCM, format.mFormatFlags & kAudioFormatFlagIsFloat != 0,
              format.mBitsPerChannel == 32, channels > 0, rate > 0
        else { return }
        let frames = sampleBuffer.numSamples
        guard frames > 0 else { return }
        do {
            try sampleBuffer.withAudioBufferList { list, _ in
                if !planar {
                    guard let first = list.first, let data = first.mData else { return }
                    let n = min(frames, Int(first.mDataByteSize) / (4 * channels))
                    if n > 0 { onSound(UnsafePointer(data.assumingMemoryBound(to: Float.self)), n, channels, rate) }
                    return
                }
                guard list.count >= channels else { return }
                let count = frames * channels
                if interleaved.count < count { interleaved = [Float](repeating: 0, count: count) }
                interleaved.withUnsafeMutableBufferPointer { out in
                    guard let base = out.baseAddress else { return }
                    for c in 0..<channels {
                        let plane = list[c]
                        let n = plane.mData == nil ? 0 : min(frames, Int(plane.mDataByteSize) / 4)
                        if let src = plane.mData?.assumingMemoryBound(to: Float.self) {
                            for i in 0..<n { base[i * channels + c] = src[i] }
                        }
                        for i in n..<frames { base[i * channels + c] = 0 }
                    }
                    onSound(UnsafePointer(base), frames, channels, rate)
                }
            }
        } catch {
            Annex.log(.trace, "[capture] couldn't read a sound chunk: \(error.localizedDescription)")
        }
    }

    func stream(_ stream: SCStream, didStopWithError error: Error) {
        Annex.log(.warn, "[capture] macOS stopped a capture: \(error.localizedDescription)")
        onStop?(error)
    }
}

enum Displays {
    struct Info: Identifiable, Hashable {
        let id: CGDirectDisplayID
        let name: String
        let pixelWidth: Int
        let pixelHeight: Int
    }

    static func all() -> [Info] {
        NSScreen.screens.compactMap { screen in
            guard let num = screen.deviceDescription[NSDeviceDescriptionKey("NSScreenNumber")] as? NSNumber else { return nil }
            let id = CGDirectDisplayID(num.uint32Value)
            let mode = CGDisplayCopyDisplayMode(id)
            return Info(id: id, name: screen.localizedName,
                        pixelWidth: mode?.pixelWidth ?? Int(screen.frame.width * screen.backingScaleFactor),
                        pixelHeight: mode?.pixelHeight ?? Int(screen.frame.height * screen.backingScaleFactor))
        }
    }

    /// Largest even size with the display's aspect ratio that fits in `maxW`×`maxH`.
    static func streamSize(pixelWidth: Int, pixelHeight: Int, maxW: Int = 1920, maxH: Int = 1200) -> (Int, Int) {
        guard pixelWidth > 0, pixelHeight > 0 else { return (1920, 1080) }
        let scale = min(1.0, min(Double(maxW) / Double(pixelWidth), Double(maxH) / Double(pixelHeight)))
        let w = Int((Double(pixelWidth) * scale).rounded(.down)) & ~1
        let h = Int((Double(pixelHeight) * scale).rounded(.down)) & ~1
        return (max(w, 2), max(h, 2))
    }

    /// Describes the main screen for a SessionRequest.
    static func mainDisplayInfo() -> DisplayInfo {
        guard let screen = NSScreen.main,
              let num = screen.deviceDescription[NSDeviceDescriptionKey("NSScreenNumber")] as? NSNumber
        else { return DisplayInfo(widthPx: 1920, heightPx: 1080) }
        let id = CGDirectDisplayID(num.uint32Value)
        let mode = CGDisplayCopyDisplayMode(id)
        let mm = CGDisplayScreenSize(id)
        // Every rate the screen offers at this size (System Settings → Displays → Refresh Rate).
        let all = (CGDisplayCopyAllDisplayModes(id, nil) as? [CGDisplayMode]) ?? []
        let rates = Set(all.filter { $0.pixelWidth == mode?.pixelWidth && $0.pixelHeight == mode?.pixelHeight }
            .map { Int($0.refreshRate.rounded()) }.filter { $0 > 1 }).sorted(by: >)
        return DisplayInfo(widthPx: mode?.pixelWidth ?? Int(screen.frame.width * screen.backingScaleFactor),
                           heightPx: mode?.pixelHeight ?? Int(screen.frame.height * screen.backingScaleFactor),
                           widthMm: Int(mm.width), heightMm: Int(mm.height),
                           refreshHz: Int((mode?.refreshRate ?? 60) > 0 ? mode!.refreshRate : 60),
                           scalePercent: Int(screen.backingScaleFactor * 100),
                           refreshRates: rates.isEmpty ? nil : rates)
    }
}
