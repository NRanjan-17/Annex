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
