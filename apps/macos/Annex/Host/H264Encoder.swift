import CoreMedia
import VideoToolbox

/// Low-latency H.264 encoder (VideoToolbox). Output is Annex B with SPS/PPS on keyframes.
///
/// By default this uses the hardware encoder with low-latency rate control (2–4 ms per frame
/// on Apple Silicon). Apple's software H.264 encoder buffers ~17 frames (~280 ms) even in
/// real-time mode and doesn't support `MaxFrameDelayCount`, so it is only a fallback for
/// machines without a usable hardware encoder (measured on macOS 27, see docs/TESTING.md).
final class H264Encoder {
    private var session: VTCompressionSession?
    private let lock = NSLock()
    private var forceKeyframe = true
    private(set) var usingHardware = false

    /// Called on a VideoToolbox thread: (Annex B access unit, keyframe, capture time µs).
    var onEncoded: ((Data, Bool, Int64) -> Void)?
    /// Called once, on any thread, when frames keep failing to encode (a second's worth in a row),
    /// so the session can end with a reason instead of showing a black screen.
    var onFailure: ((OSStatus) -> Void)?
    private var failures = 0

    /// Creating a session can take from ~0.3 s to many seconds; never call this on the main thread.
    init(width: Int, height: Int, fps: Int, bitrate: Int, preferHardware: Bool) throws {
        let hardware: [CFString: Any] = [kVTVideoEncoderSpecification_EnableLowLatencyRateControl: true]
        let software: [CFString: Any] = [kVTVideoEncoderSpecification_EnableHardwareAcceleratedVideoEncoder: false]
        let attempts = preferHardware ? [hardware, [:]] : [software, [:]]
        var status: OSStatus = -1
        var created: VTCompressionSession?
        var lowLatencyHardware = false
        for (index, spec) in attempts.enumerated() {
            status = VTCompressionSessionCreate(allocator: nil, width: Int32(width), height: Int32(height),
                                                codecType: kCMVideoCodecType_H264, encoderSpecification: spec as CFDictionary,
                                                imageBufferAttributes: nil, compressedDataAllocator: nil,
                                                outputCallback: nil, refcon: nil, compressionSessionOut: &created)
            if status == noErr {
                // Low-latency rate control exists only in the hardware encoder.
                lowLatencyHardware = preferHardware && index == 0
                break
            }
        }
        guard status == noErr, let session = created else {
            throw AnnexError(message: "Could not create H.264 encoder (OSStatus \(status))")
        }
        self.session = session

        let props: [CFString: Any] = [
            kVTCompressionPropertyKey_RealTime: true,
            kVTCompressionPropertyKey_AllowFrameReordering: false,
            kVTCompressionPropertyKey_ProfileLevel: kVTProfileLevel_H264_High_AutoLevel,
            kVTCompressionPropertyKey_AverageBitRate: bitrate,
            kVTCompressionPropertyKey_DataRateLimits: [bitrate * 3 / 2 / 8, 1] as CFArray,
            kVTCompressionPropertyKey_ExpectedFrameRate: fps,
            // Keyframes are mostly on demand (viewer join / loss); this is only a backstop.
            kVTCompressionPropertyKey_MaxKeyFrameInterval: fps * 30,
            kVTCompressionPropertyKey_MaxKeyFrameIntervalDuration: 30,
            kVTCompressionPropertyKey_ColorPrimaries: kCVImageBufferColorPrimaries_ITU_R_709_2,
            kVTCompressionPropertyKey_TransferFunction: kCVImageBufferTransferFunction_ITU_R_709_2,
            kVTCompressionPropertyKey_YCbCrMatrix: kCVImageBufferYCbCrMatrix_ITU_R_709_2,
        ]
        for (key, value) in props {
            // Not every encoder supports every key; unsupported ones are ignored.
            VTSessionSetProperty(session, key: key, value: value as CFTypeRef)
        }
        var hw: CFTypeRef?
        let copied = withUnsafeMutablePointer(to: &hw) {
            VTSessionCopyProperty(session, key: kVTCompressionPropertyKey_UsingHardwareAcceleratedVideoEncoder,
                                  allocator: nil, valueOut: $0)
        }
        // The property is not reported by low-latency sessions, hence the flag above.
        usingHardware = lowLatencyHardware || (copied == noErr && (hw as? Bool) == true)
        VTCompressionSessionPrepareToEncodeFrames(session)
    }

    deinit { invalidate() }

    func invalidate() {
        lock.lock()
        let s = session
        session = nil
        lock.unlock()
        if let s {
            VTCompressionSessionCompleteFrames(s, untilPresentationTimeStamp: .invalid)
            VTCompressionSessionInvalidate(s)
        }
    }

    /// Changes the target bitrate on the fly (adaptive bitrate).
    func setBitrate(_ bitrate: Int) {
        lock.lock()
        let s = session
        lock.unlock()
        guard let s else { return }
        VTSessionSetProperty(s, key: kVTCompressionPropertyKey_AverageBitRate, value: bitrate as CFNumber)
        VTSessionSetProperty(s, key: kVTCompressionPropertyKey_DataRateLimits, value: [bitrate * 3 / 2 / 8, 1] as CFArray)
    }

    /// Emits any frames the encoder is still holding.
    func flush() {
        lock.lock()
        let s = session
        lock.unlock()
        if let s { VTCompressionSessionCompleteFrames(s, untilPresentationTimeStamp: .invalid) }
    }

    func requestKeyframe() {
        lock.lock()
        forceKeyframe = true
        lock.unlock()
    }

    func encode(_ pixelBuffer: CVPixelBuffer, captureMicros: Int64) {
        lock.lock()
        let key = forceKeyframe
        forceKeyframe = false
        let session = self.session
        lock.unlock()
        guard let session else { return }
        let props: CFDictionary? = key ? [kVTEncodeFrameOptionKey_ForceKeyFrame: true] as CFDictionary : nil
        let pts = CMTime(value: captureMicros, timescale: 1_000_000)
        let status = VTCompressionSessionEncodeFrame(session, imageBuffer: pixelBuffer, presentationTimeStamp: pts,
                                                     duration: .invalid, frameProperties: props, infoFlagsOut: nil) { [weak self] status, _, sample in
            guard status == noErr else { self?.failed(status); return }
            self?.succeeded()
            guard let sample, let (data, isKey) = AnnexB.fromSampleBuffer(sample) else { return }
            self?.onEncoded?(data, isKey, captureMicros)
        }
        if status != noErr {
            failed(status)
            if key { requestKeyframe() }
        }
    }

    private func succeeded() {
        lock.lock()
        failures = 0
        lock.unlock()
    }

    private func failed(_ status: OSStatus) {
        lock.lock()
        failures += 1
        let report = failures == 60
        lock.unlock()
        if report { onFailure?(status) }
    }
}
