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
    let viewerFingerprint: String
    private(set) var pixelSize: (Int, Int) = (0, 0)
    /// The viewer's screen in pixels: the stream is never bigger than this.
    private var viewerPixels: (Int, Int) = (1920, 1080)
    private(set) var hardwareEncoder = false

    private let host: HostHandle
    private let display: AnnexVirtualDisplay
    /// The display's current refresh rate, which the stream follows.
    private var fps: Int
    private let preferHardware: Bool
    private let capturer = ScreenCapturer()
    private var encoder: H264Encoder?
    private let lastFrame = LockedBox<CVPixelBuffer>()
    /// Frames the encoder has produced since the last restart, for the log.
    private let encodedFrames = LockedBox<Int>()
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
        self.viewerFingerprint = viewerFingerprint
        self.preferHardware = preferHardware
        let plan = Self.plan(for: viewer)
        viewerPixels = plan.viewerPixels
        fps = plan.rates[0]
        guard let d = AnnexVirtualDisplay(
            name: "Annex Display", vendorID: Self.vendorID, productID: Self.productID,
            serial: Self.serial(for: viewerFingerprint), sizeInMillimeters: plan.millimeters,
            maxPixelsWide: UInt32(plan.maxPixels.0), maxPixelsHigh: UInt32(plan.maxPixels.1), terminationHandler: nil)
        else {
            throw AnnexError(message: "Could not create the virtual display.")
        }
        display = d
        // Every size at every rate, like a monitor: System Settings → Displays → Refresh Rate.
        let modes = plan.modes.flatMap { size in
            plan.rates.map { AnnexVirtualDisplayMode(width: UInt32(size.0), height: UInt32(size.1), refreshRate: Double($0)) }
        }
        guard d.apply(modes, hiDPI: true) else {
            d.invalidate()
            throw AnnexError(message: "macOS rejected the virtual display's modes.")
        }
        Annex.log("extend: virtual display \(d.displayID) for a \(viewer.widthPx)×\(viewer.heightPx) screen at "
                  + "\(viewer.scalePercent) %, \(plan.modes.count) sizes × rates \(plan.rates)")
        DisplayReconfiguration.shared.observe(display: d.displayID, owner: self)
        restartCapture()
    }

    struct Plan: Equatable {
        /// Point sizes, first = default.
        var modes: [(Int, Int)]
        var maxPixels: (Int, Int)
        var millimeters: CGSize
        var viewerPixels: (Int, Int)
        /// Refresh rates, first = default (the viewer's own).
        var rates: [Int]

        static func == (a: Plan, b: Plan) -> Bool {
            a.modes.elementsEqual(b.modes, by: ==) && a.maxPixels == b.maxPixels && a.millimeters == b.millimeters
                && a.viewerPixels == b.viewerPixels && a.rates == b.rates
        }
    }

    /// The scale steps Windows offers; System Settings → Displays shows them as "Larger Text" …
    /// "More Space".
    nonisolated static let scales = [100, 125, 150, 175, 200]

    /// Modes for a viewer screen: one "looks like" size per scale step, defaulting to the step
    /// nearest the viewer's own scaling (so a PC at 150 % starts at 150 %). Above 100 % macOS draws
    /// them HiDPI, at twice the point size, as on a Retina screen; the capture scales that back
    /// down to the viewer's pixels.
    nonisolated static func plan(for viewer: DisplayInfo) -> Plan {
        let w = max(viewer.widthPx, 640) & ~1
        let h = max(viewer.heightPx, 480) & ~1
        let preferred = scales.min { abs($0 - viewer.scalePercent) < abs($1 - viewer.scalePercent) }!
        let ordered = [preferred] + scales.filter { $0 != preferred }
        var modes: [(Int, Int)] = []
        for scale in ordered {
            let m = ((w * 100 / scale) & ~1, (h * 100 / scale) & ~1)
            if !modes.contains(where: { $0 == m }) && m.0 >= 640 { modes.append(m) }
        }
        // Room for 125 % at 2x (1.6 times the viewer's pixels), within what's sensible to capture.
        let maxW = max(w, min(w * 8 / 5, 6144)) & ~1
        let maxPixels = (maxW, (h * maxW / w) & ~1)
        // Physical size: what the viewer reported, else ~110 points per inch at its scaling.
        let ppi = 110.0 * Double(max(viewer.scalePercent, 100)) / 100
        let mm = viewer.widthMm > 0 && viewer.heightMm > 0
            ? CGSize(width: viewer.widthMm, height: viewer.heightMm)
            : CGSize(width: (Double(w) / ppi * 25.4).rounded(), height: (Double(h) / ppi * 25.4).rounded())
        // The rates the viewer's screen supports, its current one first (the default), like a
        // monitor's own list. Older viewers only send the current rate: add the usual lower ones.
        let hz = viewer.refreshHz > 0 ? min(max(viewer.refreshHz, 24), 240) : 60
        let supported = (viewer.refreshRates ?? []).filter { $0 >= 24 && $0 <= 240 && $0 != hz }
        let rates = [hz] + (supported.isEmpty ? [144, 120, 90, 60].filter { $0 < hz } : supported.sorted(by: >))
        return Plan(modes: modes, maxPixels: maxPixels, millimeters: mm, viewerPixels: (w, h), rates: rates)
    }

    /// The largest picture H.264 can carry (level 5.1/5.2): VideoToolbox rejects every frame of
    /// a bigger one, such as a 4.5K iMac's 4480×2520.
    nonisolated static let maxEncodeSize = (4096, 2304)

    /// What to stream for a display mode: its pixels, scaled down to fit the viewer's screen and
    /// what H.264 can carry.
    nonisolated static func streamSize(pixels: (Int, Int), viewer: (Int, Int)) -> (Int, Int) {
        let (pw, ph) = (max(pixels.0, 2), max(pixels.1, 2))
        let box = (min(viewer.0, maxEncodeSize.0), min(viewer.1, maxEncodeSize.1))
        guard pw > box.0 || ph > box.1 else { return (pw & ~1, ph & ~1) }
        let k = min(Double(box.0) / Double(pw), Double(box.1) / Double(ph))
        return (Int((Double(pw) * k).rounded()) & ~1, Int((Double(ph) * k).rounded()) & ~1)
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
        let size = Self.streamSize(pixels: (mode.pixelWidth, mode.pixelHeight), viewer: viewerPixels)
        if size != pixelSize || Self.rate(of: mode, fallback: fps) != fps { restartCapture() }
    }

    nonisolated static func rate(of mode: CGDisplayMode?, fallback: Int) -> Int {
        guard let hz = mode?.refreshRate, hz > 0 else { return fallback }
        return Int(hz.rounded())
    }

    private func restartCapture() {
        guard !stopped else { return }
        generation += 1
        let gen = generation
        let id = display.displayID
        let mode = CGDisplayCopyDisplayMode(id)
        let size = Self.streamSize(pixels: (mode?.pixelWidth ?? viewerPixels.0, mode?.pixelHeight ?? viewerPixels.1),
                                   viewer: viewerPixels)
        pixelSize = size
        fps = Self.rate(of: mode, fallback: fps)
        capturer.onFrame = nil
        encoder?.invalidate()
        encoder = nil
        host.setStream(session: sessionID, width: size.0, height: size.1, fps: fps)
        Annex.log("extend: display mode \(mode?.width ?? 0)×\(mode?.height ?? 0) points, "
                  + "\(mode?.pixelWidth ?? 0)×\(mode?.pixelHeight ?? 0) pixels; streaming \(size.0)×\(size.1) at \(fps) fps")
        let (fps, preferHardware, capturer) = (self.fps, self.preferHardware, self.capturer)
        // Frames get more alike as the rate goes up, so above 60 fps the bitrate grows half as fast.
        let bitrate = max(6_000_000, size.0 * size.1 * (min(fps, 60) + max(fps - 60, 0) / 2) / 8)
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
        Annex.log("extend: encoder ready (\(enc.usingHardware ? "hardware" : "software"))")
        encodedFrames.value = 0
        hardwareEncoder = enc.usingHardware
        if bitrateScale != 100 { enc.setBitrate(baseBitrate * bitrateScale / 100) }
        let (host, session) = (self.host, sessionID)
        let encodedFrames = self.encodedFrames
        enc.onEncoded = { data, key, ts in
            encodedFrames.update { $0 = ($0 ?? 0) + 1 }
            host.submit(data, session: session, captureMicros: ts, keyframe: key)
        }
        enc.onFailure = { [weak self] status in
            Task { @MainActor in
                guard let self, gen == self.generation, !self.stopped else { return }
                self.onError?("The video encoder couldn't encode the extended display (error \(status)).")
            }
        }
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
                Annex.log("extend: capture started")
                expectFirstFrame(generation: gen)
                return
            } catch {
                if attempt == 19 { onError?("Capturing the virtual display failed: \(error.localizedDescription)") }
                try? await Task.sleep(nanoseconds: 100_000_000)
            }
        }
    }
}

extension ExtendSession {
    /// A new capture always delivers a first frame quickly; if none arrives, the viewer would sit
    /// on a black window while the Mac stays extended, so the session fails instead.
    fileprivate func expectFirstFrame(generation gen: Int) {
        lastFrame.value = nil
        Task { @MainActor [weak self] in
            try? await Task.sleep(nanoseconds: 6_000_000_000)
            guard let self, gen == self.generation, !self.stopped else { return }
            let encoded = self.encodedFrames.value ?? 0
            guard self.lastFrame.value == nil else {
                Annex.log("extend: picture arriving, \(encoded) frames encoded in the first 6 s")
                return
            }
            self.onError?("The extended display sent no picture. Check Screen Recording for Annex in System Settings.")
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
