@testable import Annex
import XCTest

final class AnnexBTests: XCTestCase {
    func testNalSplittingHandlesThreeAndFourByteStartCodes() {
        let bytes: [UInt8] = [0, 0, 0, 1, 0x67, 0x42, 0, 0, 1, 0x68, 0xCE, 0, 0, 0, 1, 0x65, 0x88]
        let nals = AnnexB.nalUnits(bytes).map(Array.init)
        XCTAssertEqual(nals, [[0x67, 0x42], [0x68, 0xCE], [0x65, 0x88]])
        XCTAssertEqual(AnnexB.nalType(AnnexB.nalUnits(bytes)[2]), 5)
    }

    func testAVCCRoundTrip() {
        let avcc: [UInt8] = [0, 0, 0, 2, 0x65, 0x01, 0, 0, 0, 3, 0x41, 0x02, 0x03]
        let annexB = AnnexB.fromAVCC(avcc)!
        XCTAssertEqual(annexB, [0, 0, 0, 1, 0x65, 0x01, 0, 0, 0, 1, 0x41, 0x02, 0x03])
        XCTAssertEqual(AnnexB.toAVCC(AnnexB.nalUnits(annexB)), avcc)
    }

    func testMalformedAVCCIsRejected() {
        XCTAssertNil(AnnexB.fromAVCC([0, 0, 0, 9, 0x65]))
        XCTAssertNil(AnnexB.fromAVCC([0, 0]))
    }

    func testParameterSetsAreDroppedFromAVCC() {
        let annexB: [UInt8] = [0, 0, 0, 1, 0x67, 1, 0, 0, 0, 1, 0x68, 2, 0, 0, 0, 1, 0x09, 0xF0, 0, 0, 0, 1, 0x65, 3]
        XCTAssertEqual(AnnexB.toAVCC(AnnexB.nalUnits(annexB)), [0, 0, 0, 2, 0x65, 3])
    }

    func testStreamSizeFitsAndIsEven() {
        XCTAssertEqual(Displays.streamSize(pixelWidth: 3024, pixelHeight: 1964).0, 1846)
        let (w, h) = Displays.streamSize(pixelWidth: 2560, pixelHeight: 1440)
        XCTAssertEqual(w, 1920)
        XCTAssertEqual(h, 1080)
        XCTAssertEqual(Displays.streamSize(pixelWidth: 1280, pixelHeight: 800).0, 1280)
        XCTAssertEqual(Displays.streamSize(pixelWidth: 1921, pixelHeight: 1081).0 % 2, 0)
    }

    func testExtendPlanForRetinaAndStandardViewers() {
        let retina = ExtendSession.plan(for: DisplayInfo(widthPx: 2880, heightPx: 1800, widthMm: 0, heightMm: 0, refreshHz: 60, scalePercent: 200))
        XCTAssertEqual(retina.modes.first!.0, 1440, "Retina viewers default to a 2x HiDPI mode")
        XCTAssertEqual(retina.modes.first!.1, 900)
        XCTAssertEqual(retina.maxPixels.0, 2880)
        XCTAssertEqual(retina.maxPixels.1, 1800)
        XCTAssertGreaterThan(retina.millimeters.width, 300)
        let standard = ExtendSession.plan(for: DisplayInfo(widthPx: 1920, heightPx: 1080, widthMm: 527, heightMm: 296, refreshHz: 60, scalePercent: 100))
        XCTAssertEqual(standard.modes.first!.0, 1920)
        XCTAssertEqual(standard.modes.first!.1, 1080)
        XCTAssertEqual(standard.millimeters, CGSize(width: 527, height: 296))
        XCTAssertTrue(standard.modes.allSatisfy { $0.0 % 2 == 0 && $0.1 % 2 == 0 })
        XCTAssertEqual(ExtendSession.serial(for: "deadbeef00"), 0xDEADBEEF)
        XCTAssertEqual(ExtendSession.serial(for: "zz"), 1)
    }

    func testEventDecoding() {
        let ended = #"{"type":"session_ended","session_id":3,"reason":"user"}"#
        XCTAssertEqual(CoreEvent.decode(Data(ended.utf8)), .sessionEnded(sessionId: 3, reason: "user"))
        let json = #"{"type":"latency_stats","frames":3,"fps":60.0,"p50_ms":12.5,"p95_ms":20,"p99_ms":21,"max_ms":30,"clock_rtt_ms":0.4}"#
        guard case .latencyStats(let l) = CoreEvent.decode(Data(json.utf8)) else { return XCTFail("wrong case") }
        XCTAssertEqual(l.p50Ms, 12.5)
        XCTAssertEqual(l.clockRttMs, 0.4)
        let peer = #"{"type":"peer_found","instance":"A._annex._tcp.local.","name":"A","addresses":["fe80::1"],"port":47800,"fingerprint":null,"proto_major":1}"#
        guard case .peerFound(let p) = CoreEvent.decode(Data(peer.utf8)) else { return XCTFail("wrong case") }
        XCTAssertEqual(p.connectAddress, "[fe80::1]:47800")
        let multi = PeerInfo(instance: "x", name: "x", addresses: ["192.168.1.5", "169.254.3.4"], port: 9, fingerprint: nil, protoMajor: nil)
        XCTAssertEqual(multi.connectAddress, "192.168.1.5:9,169.254.3.4:9")
        let hint = #"{"type":"bitrate_hint","session_id":2,"scale_percent":80}"#
        XCTAssertEqual(CoreEvent.decode(Data(hint.utf8)), .bitrateHint(sessionId: 2, scalePercent: 80))
        XCTAssertEqual(CoreEvent.decode(Data(#"{"type":"from_future"}"#.utf8)), .unknown(type: "from_future"))
        let pin = #"{"type":"pairing_pin","request_id":4,"pin":"123 456","peer_name":"PC","fingerprint_display":"AB12"}"#
        XCTAssertEqual(CoreEvent.decode(Data(pin.utf8)),
                       .pairingPin(PairingPin(requestId: 4, pin: "123 456", peerName: "PC", fingerprintDisplay: "AB12")))
    }
}
