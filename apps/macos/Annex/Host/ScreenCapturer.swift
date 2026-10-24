import CoreMedia
import ScreenCaptureKit

/// Captures one display with ScreenCaptureKit, scaled by the system to the stream size and
/// delivered as NV12 (420v) pixel buffers ready for VideoToolbox.
final class ScreenCapturer: NSObject, SCStreamOutput, SCStreamDelegate {
    // start/stop run off the caller's actor and may overlap, so the stream and a generation
    // (bumped by every start and stop) are guarded by a lock.
    private let lock = NSLock()
    private var stream: SCStream?
    private var generation = 0
    private let queue = DispatchQueue(label: "annex.capture", qos: .userInteractive)

    /// Called on the capture queue for every new frame.
    var onFrame: ((CVPixelBuffer) -> Void)?
    /// Called if the system stops the stream (permission revoked, display removed, ...).
    var onStop: ((Error) -> Void)?

    /// Replaces any running capture. Returns without capturing if `stop()` or another `start` was
    /// called meanwhile, so a stop during start can never leave capture running.
    func start(displayID: CGDirectDisplayID, width: Int, height: Int, fps: Int) async throws {
        let (gen, old) = lock.withLock { () -> (Int, SCStream?) in
            generation += 1
            defer { stream = nil }
            return (generation, stream)
        }
        if let old { try? await old.stopCapture() }
        let content = try await SCShareableContent.excludingDesktopWindows(false, onScreenWindowsOnly: true)
        guard let display = content.displays.first(where: { $0.displayID == displayID }) else {
            throw AnnexError(message: "Display \(displayID) is not available for capture")
        }
        let filter = SCContentFilter(display: display, excludingWindows: [])
        let config = SCStreamConfiguration()
        config.width = width
        config.height = height
        config.minimumFrameInterval = CMTime(value: 1, timescale: CMTimeScale(fps))
        config.pixelFormat = kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange
        config.colorMatrix = CGDisplayStream.yCbCrMatrix_ITU_R_709_2
        config.showsCursor = true
        config.queueDepth = 4
        config.capturesAudio = false

        guard isCurrent(gen) else { return }
        let stream = SCStream(filter: filter, configuration: config, delegate: self)
        try stream.addStreamOutput(self, type: .screen, sampleHandlerQueue: queue)
        do {
            try await stream.startCapture()
        } catch {
            if isCurrent(gen) { throw error }
            return
        }
        let current = lock.withLock { () -> Bool in
            guard gen == generation else { return false }
            self.stream = stream
            return true
        }
        if !current { try? await stream.stopCapture() }
    }

    func stop() async {
        let stream = lock.withLock { () -> SCStream? in
            generation += 1
            defer { self.stream = nil }
            return self.stream
        }
        try? await stream?.stopCapture()
    }

    private func isCurrent(_ gen: Int) -> Bool {
        lock.withLock { gen == generation }
    }

    func stream(_ stream: SCStream, didOutputSampleBuffer sampleBuffer: CMSampleBuffer, of type: SCStreamOutputType) {
        guard type == .screen, sampleBuffer.isValid,
              let attachments = CMSampleBufferGetSampleAttachmentsArray(sampleBuffer, createIfNecessary: false) as? [[SCStreamFrameInfo: Any]],
              let rawStatus = attachments.first?[.status] as? Int,
              SCFrameStatus(rawValue: rawStatus) == .complete,
              let pixelBuffer = sampleBuffer.imageBuffer
        else { return }
        onFrame?(pixelBuffer)
    }

    func stream(_ stream: SCStream, didStopWithError error: Error) {
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
