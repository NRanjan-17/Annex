@testable import Annex
import CoreVideo
import VideoToolbox
import XCTest

/// Encodes synthetic frames with the M1 software encoder, converts to Annex B (the wire
/// format), rebuilds CMSampleBuffers like the viewer does, and decodes them again.
final class EncoderRoundTripTests: XCTestCase {
    private func makeFrame(width: Int, height: Int, shade: UInt8) -> CVPixelBuffer {
        var pb: CVPixelBuffer?
        let attrs = [kCVPixelBufferIOSurfacePropertiesKey: [:]] as CFDictionary
        CVPixelBufferCreate(nil, width, height, kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange, attrs, &pb)
        let buffer = pb!
        CVPixelBufferLockBaseAddress(buffer, [])
        for plane in 0..<CVPixelBufferGetPlaneCount(buffer) {
            let base = CVPixelBufferGetBaseAddressOfPlane(buffer, plane)!
            let size = CVPixelBufferGetBytesPerRowOfPlane(buffer, plane) * CVPixelBufferGetHeightOfPlane(buffer, plane)
            memset(base, Int32(plane == 0 ? shade : 128), size)
        }
        CVPixelBufferUnlockBaseAddress(buffer, [])
        return buffer
    }

    /// Hardware encoder when available (falls back to software on machines without one).
    func testEncodeDecodeRoundTrip() throws {
        try roundTrip(preferHardware: true)
    }

    /// Apple's software encoder, used when the hardware one is unavailable.
    func testSoftwareEncodeDecodeRoundTrip() throws {
        try roundTrip(preferHardware: false)
    }

    /// A 4.5K iMac's Extend stream at 60 fps: the low-latency hardware encoder returns no frames
    /// (and no error) above H.264 level 5.2's macroblock rate, so the stream must stay within it.
    func testLargestExtendStreamEncodes() throws {
        let (w, h) = ExtendSession.streamSize(pixels: (4480, 2520), viewer: (4480, 2520), fps: 60)
        let encoder = try H264Encoder(width: w, height: h, fps: 60, bitrate: w * h * 60 / 8, preferHardware: true)
        var count = 0
        let lock = NSLock()
        encoder.onEncoded = { _, _, _ in lock.lock(); count += 1; lock.unlock() }
        for i in 0..<8 {
            encoder.encode(makeFrame(width: w, height: h, shade: UInt8(16 + i * 18)), captureMicros: Int64(i) * 16_667)
            usleep(16_667)
        }
        encoder.flush()
        encoder.invalidate()
        lock.lock(); defer { lock.unlock() }
        XCTAssertGreaterThanOrEqual(count, 6, "\(w)×\(h) at 60 fps encoded only \(count) of 8 frames")
    }

    /// HEVC between Macs: keyframes carry VPS/SPS/PPS, every frame decodes, and the viewer's
    /// builder follows the stream switching to H.264 (a viewer that needs it joined) and back.
    func testHEVCEncodeDecodeRoundTrip() throws {
        guard H264SampleBuilder.hardwareDecodesHEVC else { throw XCTSkip("this Mac doesn't decode HEVC in hardware") }
        let (w, h) = (640, 360)
        let units = try encode(width: w, height: h, preferHardware: true, codec: "hevc")
        XCTAssertGreaterThanOrEqual(units.count, 6, "too many frames dropped")
        XCTAssertTrue(units[0].1, "first frame must be a keyframe")
        let firstNals = AnnexB.nalUnits([UInt8](units[0].0)).map { AnnexB.hevcNalType($0) }
        XCTAssertTrue(firstNals.contains(32) && firstNals.contains(33) && firstNals.contains(34),
                      "keyframe carries VPS, SPS, PPS: \(firstNals)")

        var builder = H264SampleBuilder()
        let samples = units.compactMap { builder.sampleBuffer(from: $0.0) }
        XCTAssertTrue(builder.hevc)
        XCTAssertEqual(samples.count, units.count)
        let dims = try XCTUnwrap(builder.dimensions)
        XCTAssertEqual(Int(dims.width), w)
        XCTAssertEqual(Int(dims.height), h)
        XCTAssertEqual(decode(samples, format: builder.format!), samples.count)

        // Switched to H.264 mid-stream, then back to HEVC: the builder follows each keyframe.
        let h264 = try encode(width: w, height: h, preferHardware: true, codec: "h264")
        XCTAssertNotNil(builder.sampleBuffer(from: h264[0].0))
        XCTAssertFalse(builder.hevc)
        XCTAssertNotNil(builder.sampleBuffer(from: units[0].0))
        XCTAssertTrue(builder.hevc)
    }

    /// Encodes 12 frames (a keyframe asked for half-way) and returns (Annex B unit, keyframe).
    private func encode(width w: Int, height h: Int, preferHardware: Bool, codec: String) throws -> [(Data, Bool)] {
        let encoder = try H264Encoder(width: w, height: h, fps: 60, bitrate: 2_000_000, preferHardware: preferHardware,
                                      codec: codec)
        var units: [(Data, Bool)] = []
        let lock = NSLock()
        encoder.onEncoded = { data, key, _ in
            lock.lock(); units.append((data, key)); lock.unlock()
        }
        for i in 0..<12 {
            if i == 6 { encoder.requestKeyframe() }
            encoder.encode(makeFrame(width: w, height: h, shade: UInt8(16 + i * 18)), captureMicros: Int64(i) * 16_667)
            usleep(16_667)
        }
        encoder.flush()
        encoder.invalidate()
        lock.lock(); defer { lock.unlock() }
        return units
    }

    /// Decodes `samples` and returns how many came out as pictures.
    private func decode(_ samples: [CMSampleBuffer], format: CMVideoFormatDescription) -> Int {
        var session: VTDecompressionSession?
        guard VTDecompressionSessionCreate(allocator: nil, formatDescription: format, decoderSpecification: nil,
                                           imageBufferAttributes: nil, outputCallback: nil, decompressionSessionOut: &session) == noErr,
              let session else { return 0 }
        var decoded = 0
        for s in samples {
            VTDecompressionSessionDecodeFrame(session, sampleBuffer: s, flags: [], infoFlagsOut: nil) { status, _, image, _, _ in
                if status == noErr, image != nil { decoded += 1 }
            }
        }
        VTDecompressionSessionWaitForAsynchronousFrames(session)
        VTDecompressionSessionInvalidate(session)
        return decoded
    }

    private func roundTrip(preferHardware: Bool) throws {
        let (w, h) = (640, 360)
        let encoder = try H264Encoder(width: w, height: h, fps: 60, bitrate: 2_000_000, preferHardware: preferHardware)
        var units: [(Data, Bool)] = []
        let lock = NSLock()
        encoder.onEncoded = { data, key, _ in
            lock.lock(); units.append((data, key)); lock.unlock()
        }
        for i in 0..<12 {
            if i == 6 { encoder.requestKeyframe() }
            encoder.encode(makeFrame(width: w, height: h, shade: UInt8(16 + i * 18)), captureMicros: Int64(i) * 16_667)
            usleep(16_667)
        }
        encoder.flush()
        encoder.invalidate()

        lock.lock(); defer { lock.unlock() }
        XCTAssertGreaterThanOrEqual(units.count, 6, "too many frames dropped")
        XCTAssertTrue(units[0].1, "first frame must be a keyframe")
        XCTAssertTrue(units.dropFirst().contains { $0.1 }, "requested keyframe must be honoured")
        XCTAssertTrue(units.contains { !$0.1 }, "expected delta frames")
        let firstNals = AnnexB.nalUnits([UInt8](units[0].0)).map { AnnexB.nalType($0) }
        XCTAssertTrue(firstNals.contains(7) && firstNals.contains(8) && firstNals.contains(5), "keyframe carries SPS, PPS, IDR: \(firstNals)")

        var builder = H264SampleBuilder()
        let samples = units.compactMap { builder.sampleBuffer(from: $0.0) }
        XCTAssertEqual(samples.count, units.count)
        let dims = try XCTUnwrap(builder.dimensions)
        XCTAssertEqual(Int(dims.width), w)
        XCTAssertEqual(Int(dims.height), h)

        var session: VTDecompressionSession?
        XCTAssertEqual(VTDecompressionSessionCreate(allocator: nil, formatDescription: builder.format!, decoderSpecification: nil,
                                                    imageBufferAttributes: nil, outputCallback: nil, decompressionSessionOut: &session), noErr)
        var decoded = 0
        for s in samples {
            VTDecompressionSessionDecodeFrame(session!, sampleBuffer: s, flags: [], infoFlagsOut: nil) { status, _, image, _, _ in
                if status == noErr, image != nil { decoded += 1 }
            }
        }
        VTDecompressionSessionWaitForAsynchronousFrames(session!)
        VTDecompressionSessionInvalidate(session!)
        XCTAssertEqual(decoded, samples.count)
    }
}
