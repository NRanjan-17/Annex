import CoreGraphics
import CoreVideo
import Foundation

/// One Extend session: a virtual display created for a viewer, captured and encoded for that
/// viewer only. The display follows the viewer's native resolution by default; if the user
/// picks another mode in System Settings → Displays, capture restarts at the new size and the
/// viewer is told (usually well under a second).
@MainActor
final class ExtendSession {
    let sessionID: UInt64
    let viewerName: String
    private(set) var pixelSize: (Int, Int) = (0, 0)
    private(set) var hardwareEncoder = false

    private let host: HostHandle
    private let display: AnnexVirtualDisplay
    private let fps: Int
    private let preferHardware: Bool
    private let capturer = ScreenCapturer()
    private var encoder: H264Encoder?
    private let lastFrame = LockedBox<CVPixelBuffer>()
    private var generation = 0
    private var stopped = false

    var onError: ((String) -> Void)?

    /// The virtual display this viewer extends to (where its pointer lands).
    var displayID: CGDirectDisplayID { display.displayID }

    /// Tests turn capture off so they don't need the Screen Recording permission.
    static var captureEnabled = true

    /// Vendor/product shown in System Settings; the serial is per viewer (stable identity).
    static let vendorID: UInt32 = 0x4E41  // "AN"
    static let productID: UInt32 = 0x4558  // "EX"

    init(sessionID: UInt64, host: HostHandle, viewer: DisplayInfo, viewerFingerprint: String, viewerName: String,
         preferHardware: Bool) throws {
        guard AnnexVirtualDisplay.isSupported() else {
            throw AnnexError(message: "Virtual displays aren't supported on this version of macOS.")
        }
        self.sessionID = sessionID
        self.host = host
        self.viewerName = viewerName
        self.preferHardware = preferHardware
        fps = viewer.refreshHz > 0 ? min(max(viewer.refreshHz, 24), 60) : 60
        let plan = Self.plan(for: viewer)
        guard let d = AnnexVirtualDisplay(
            name: "Annex Display", vendorID: Self.vendorID, productID: Self.productID,
            serial: Self.serial(for: viewerFingerprint), sizeInMillimeters: plan.millimeters,
            maxPixelsWide: UInt32(plan.maxPixels.0), maxPixelsHigh: UInt32(plan.maxPixels.1), terminationHandler: nil)
        else {
            throw AnnexError(message: "Could not create the virtual display.")
        }
        display = d
        let modes = plan.modes.map { AnnexVirtualDisplayMode(width: UInt32($0.0), height: UInt32($0.1), refreshRate: Double(fps)) }
        guard d.apply(modes, hiDPI: true) else {
            d.invalidate()
            throw AnnexError(message: "macOS rejected the virtual display's modes.")
        }
        DisplayReconfiguration.shared.observe(display: d.displayID, owner: self)
        restartCapture()
    }

    struct Plan: Equatable {
        /// Point sizes, first = default.
        var modes: [(Int, Int)]
        var maxPixels: (Int, Int)
        var millimeters: CGSize

        static func == (a: Plan, b: Plan) -> Bool {
            a.modes.elementsEqual(b.modes, by: ==) && a.maxPixels == b.maxPixels && a.millimeters == b.millimeters
        }
    }

    /// Modes for a viewer screen: Retina viewers (≥ 150 %) default to a 2x HiDPI mode at their
    /// native pixel size; others to their native resolution at 1x. macOS adds the usual scaled
    /// variants itself.
    nonisolated static func plan(for viewer: DisplayInfo) -> Plan {
        let w = max(viewer.widthPx, 640) & ~1
        let h = max(viewer.heightPx, 480) & ~1
        let retina = viewer.scalePercent >= 150
        var modes: [(Int, Int)] = retina ? [(w / 2, h / 2), (w, h)] : [(w, h)]
        // A couple of common smaller sizes with the same aspect ratio.
        for scale in [0.75, 0.5] {
            let m = (Int(Double(w) * scale) & ~1, Int(Double(h) * scale) & ~1)
            if !modes.contains(where: { $0 == m }) && m.0 >= 640 { modes.append(m) }
        }
        // Physical size: what the viewer reported, else ~110 points per inch.
        let ppi = retina ? 220.0 : 110.0
        let mm = viewer.widthMm > 0 && viewer.heightMm > 0
            ? CGSize(width: viewer.widthMm, height: viewer.heightMm)
            : CGSize(width: (Double(w) / ppi * 25.4).rounded(), height: (Double(h) / ppi * 25.4).rounded())
        return Plan(modes: modes, maxPixels: (w, h), millimeters: mm)
    }

    /// Stable 32-bit serial from the viewer's certificate fingerprint.
    nonisolated static func serial(for fingerprint: String) -> UInt32 {
        UInt32(fingerprint.prefix(8), radix: 16) ?? 1
    }

    private var bitrateScale = 100
    private var baseBitrate = 0

    /// Adaptive bitrate from the viewer's reports.
    func setBitrateScale(_ percent: Int) {
        bitrateScale = percent
        if baseBitrate > 0 { encoder?.setBitrate(baseBitrate * percent / 100) }
    }

    func requestKeyframe() {
        encoder?.requestKeyframe()
        if let pb = lastFrame.value { encoder?.encode(pb, captureMicros: Annex.nowMicros()) }
    }

    /// Removes the virtual display; macOS moves its windows back to the remaining displays.
    func stop() {
        guard !stopped else { return }
        stopped = true
        generation += 1
        DisplayReconfiguration.shared.forget(display: display.displayID)
        capturer.onFrame = nil
        let capturer = self.capturer
        Task { await capturer.stop() }
        encoder?.invalidate()
        encoder = nil
        display.invalidate()
    }

    /// Called when the display's mode may have changed.
    func reconfigured() {
        guard !stopped, let mode = CGDisplayCopyDisplayMode(display.displayID) else { return }
        let size = (mode.pixelWidth & ~1, mode.pixelHeight & ~1)
        if size != pixelSize { restartCapture() }
    }

    private func restartCapture() {
        guard !stopped else { return }
        generation += 1
        let gen = generation
        let id = display.displayID
        let mode = CGDisplayCopyDisplayMode(id)
        let size = ((mode?.pixelWidth ?? 1920) & ~1, (mode?.pixelHeight ?? 1080) & ~1)
        pixelSize = size
        capturer.onFrame = nil
        encoder?.invalidate()
        encoder = nil
        host.setStream(session: sessionID, width: size.0, height: size.1, fps: fps)
        let (fps, preferHardware, capturer) = (self.fps, self.preferHardware, self.capturer)
        let bitrate = max(6_000_000, size.0 * size.1 * fps / 8)
        baseBitrate = bitrate
        Task.detached(priority: .userInitiated) { [weak self] in
            let result = Result { try H264Encoder(width: size.0, height: size.1, fps: fps, bitrate: bitrate, preferHardware: preferHardware) }
            await self?.encoderReady(result, generation: gen, size: size, capturer: capturer, displayID: id)
        }
    }

    private func encoderReady(_ result: Result<H264Encoder, Error>, generation gen: Int, size: (Int, Int),
                              capturer: ScreenCapturer, displayID: CGDirectDisplayID) async {
        guard gen == generation, !stopped else {
            if case .success(let e) = result { e.invalidate() }
            return
        }
        let enc: H264Encoder
        switch result {
        case .success(let e): enc = e
        case .failure(let e): onError?(e.localizedDescription); return
        }
        hardwareEncoder = enc.usingHardware
        if bitrateScale != 100 { enc.setBitrate(baseBitrate * bitrateScale / 100) }
        let (host, session) = (self.host, sessionID)
        enc.onEncoded = { data, key, ts in host.submit(data, session: session, captureMicros: ts, keyframe: key) }
        encoder = enc
        let lastFrame = self.lastFrame
        capturer.onFrame = { [weak enc] pb in
            lastFrame.value = pb
            enc?.encode(pb, captureMicros: Annex.nowMicros())
        }
        await capturer.stop()
        guard Self.captureEnabled else { return }
        // A brand-new display can take a moment to show up in ScreenCaptureKit.
        for attempt in 0..<20 {
            guard gen == generation, !stopped else { return }
            do {
                try await capturer.start(displayID: displayID, width: size.0, height: size.1, fps: fps)
                return
            } catch {
                if attempt == 19 { onError?("Capturing the virtual display failed: \(error.localizedDescription)") }
                try? await Task.sleep(nanoseconds: 100_000_000)
            }
        }
    }
}

/// Routes CoreGraphics display reconfiguration callbacks to the ExtendSession that owns a display.
@MainActor
final class DisplayReconfiguration {
    static let shared = DisplayReconfiguration()
    private var owners: [CGDirectDisplayID: WeakSession] = [:]
    private var registered = false

    private struct WeakSession { weak var session: ExtendSession? }

    func observe(display: CGDirectDisplayID, owner: ExtendSession) {
        owners[display] = WeakSession(session: owner)
        guard !registered else { return }
        registered = true
        CGDisplayRegisterReconfigurationCallback({ display, flags, _ in
            // Only act once the change is complete.
            guard !flags.contains(.beginConfigurationFlag) else { return }
            DispatchQueue.main.async { DisplayReconfiguration.shared.changed(display) }
        }, nil)
    }

    func forget(display: CGDirectDisplayID) {
        owners.removeValue(forKey: display)
    }

    private func changed(_ display: CGDirectDisplayID) {
        owners[display]?.session?.reconfigured()
    }
}
