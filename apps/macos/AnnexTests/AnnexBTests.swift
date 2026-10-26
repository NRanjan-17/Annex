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
        XCTAssertGreaterThanOrEqual(retina.maxPixels.0, 2880, "room for the default mode at 2x")
        XCTAssertGreaterThan(retina.millimeters.width, 300)
        let standard = ExtendSession.plan(for: DisplayInfo(widthPx: 1920, heightPx: 1080, widthMm: 527, heightMm: 296, refreshHz: 60, scalePercent: 100))
        XCTAssertEqual(standard.modes.first!.0, 1920)
        XCTAssertEqual(standard.modes.first!.1, 1080)
        XCTAssertEqual(standard.millimeters, CGSize(width: 527, height: 296))
        XCTAssertTrue(standard.modes.allSatisfy { $0.0 % 2 == 0 && $0.1 % 2 == 0 })
        // Every Windows scale step is offered; a PC at 125 % starts at a sharp 100 %, a Retina one at 200 %.
        XCTAssertEqual(standard.modes.map(\.0), [1920, 1536, 1280, 1096, 960])
        XCTAssertEqual(standard.maxPixels.0, 3072, "125 % drawn at 2x fits")
        let laptop = ExtendSession.plan(for: DisplayInfo(widthPx: 1920, heightPx: 1080, widthMm: 0, heightMm: 0, refreshHz: 60, scalePercent: 125))
        XCTAssertEqual(laptop.modes.first!.0, 1920)
        XCTAssertEqual(laptop.modes.first!.1, 1080)
        XCTAssertTrue(laptop.modes.contains { $0 == (1536, 864) }, "125 % can still be chosen")
        // Settings → Sharing → Extended display scaling: its step comes first; Sharpest depends on the screen.
        let chosen = ExtendSession.plan(for: DisplayInfo(widthPx: 1920, heightPx: 1080, scalePercent: 125), scale: 150)
        XCTAssertEqual(chosen.modes.first!.0, 1280)
        XCTAssertEqual(ExtendSession.resolvedScale(0, viewerScalePercent: 125), 100)
        XCTAssertEqual(ExtendSession.resolvedScale(0, viewerScalePercent: 200), 200)
        XCTAssertEqual(ExtendSession.resolvedScale(175, viewerScalePercent: 100), 175)
        XCTAssertEqual(ExtendSession.resolvedScale(130, viewerScalePercent: 100), 100, "not a step")
        // Refresh rates: the viewer's list, current first; without one, the usual lower rates.
        XCTAssertEqual(standard.rates, [60])
        let fast = ExtendSession.plan(for: DisplayInfo(widthPx: 1920, heightPx: 1080, refreshHz: 144, refreshRates: [165, 144, 120, 60, 48]))
        XCTAssertEqual(fast.rates, [144, 165, 120, 60, 48])
        let old = ExtendSession.plan(for: DisplayInfo(widthPx: 1920, heightPx: 1080, refreshHz: 144))
        XCTAssertEqual(old.rates, [144, 120, 90, 60])
        // A scaled mode is drawn bigger than the viewer's screen and streamed at its size.
        XCTAssertTrue(ExtendSession.streamSize(pixels: (3072, 1728), viewer: (1920, 1080), fps: 60) == (1920, 1080))
        XCTAssertTrue(ExtendSession.streamSize(pixels: (1280, 720), viewer: (1920, 1080), fps: 60) == (1280, 720))
        // A 4.5K iMac at 2x: more than H.264 can carry, so it's streamed at 4096×2304 at 30 fps…
        XCTAssertTrue(ExtendSession.streamSize(pixels: (4480, 2520), viewer: (4480, 2520), fps: 30) == (4096, 2304))
        // …and smaller at 60 fps, within level 5.2's macroblock rate.
        let big = ExtendSession.streamSize(pixels: (4480, 2520), viewer: (4480, 2520), fps: 60)
        XCTAssertLessThanOrEqual(((big.0 + 15) / 16) * ((big.1 + 15) / 16) * 60, ExtendSession.maxMacroblocksPerSecond)
        XCTAssertGreaterThan(big.0, 3800)
        // A 3840×2160 screen at 60 fps already fits and keeps its size.
        XCTAssertTrue(ExtendSession.streamSize(pixels: (3840, 2160), viewer: (3840, 2160), fps: 60) == (3840, 2160))
        XCTAssertEqual(ExtendSession.serial(for: "deadbeef00"), 0xDEADBEEF)
        XCTAssertEqual(ExtendSession.serial(for: "zz"), 1)
    }

    @MainActor func testUnreachableReasonsPointAtTheFix() {
        // A timeout can be a network that keeps computers apart: point at Direct Wi-Fi.
        XCTAssertTrue(AppModel.friendlyReason("timed out: connect", peer: "PC").contains("Direct Wi-Fi to a PC"))
        // Refused means the other computer answered: it isn't sharing, so no Direct Wi-Fi hint.
        let refused = AppModel.friendlyReason("i/o: Connection refused (os error 61)", peer: "PC")
        XCTAssertTrue(refused.hasPrefix("Couldn't reach PC."))
        XCTAssertFalse(refused.contains("Direct Wi-Fi"))
    }

    @MainActor func testCodeFailuresSayWhatToDo() {
        XCTAssertTrue(AppModel.friendlyReason("unknown_code", peer: "x").contains("ask for a new one"))
        // Over the internet a timeout means no path, not "same network".
        let lost = AppModel.friendlyReason("timed out: quic handshake", peer: "x", byCode: true)
        XCTAssertTrue(lost.contains("relay"))
        XCTAssertFalse(lost.contains("same network"))
    }

    func testInternetEventDecoding() {
        func decode(_ s: String) -> CoreEvent { CoreEvent.decode(Data(s.utf8)) }
        XCTAssertEqual(decode(#"{"type":"internet_code","code":"K7P-4QX","expires_in":600}"#),
                       .internetCode(code: "K7P-4QX", expiresIn: 600))
        XCTAssertEqual(decode(#"{"type":"internet_joined","peer_name":"Office PC"}"#), .internetJoined(peerName: "Office PC"))
        XCTAssertEqual(decode(#"{"type":"internet_relay","relayed":true,"limited":true}"#),
                       .internetRelay(relayed: true, limited: true))
        XCTAssertEqual(decode(#"{"type":"internet_code_ended","reason":"expired"}"#), .internetCodeEnded(reason: "expired"))
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
        // Older peers don't send the flags: they only advertised while sharing.
        XCTAssertTrue(p.isSharing)
        XCTAssertTrue(p.canExtend)
        let idle = #"{"type":"peer_found","instance":"B","name":"B","addresses":[],"port":47800,"fingerprint":null,"proto_major":1,"sharing":false,"extend":false}"#
        guard case .peerFound(let q) = CoreEvent.decode(Data(idle.utf8)) else { return XCTFail("wrong case") }
        XCTAssertFalse(q.isSharing)
        XCTAssertFalse(q.canExtend)
        let multi = PeerInfo(instance: "x", name: "x", addresses: ["192.168.1.5", "169.254.3.4"], port: 9, fingerprint: nil,
                             protoMajor: nil, sharing: true, extend: nil)
        XCTAssertEqual(multi.connectAddress, "192.168.1.5:9,169.254.3.4:9")
        var mesh = multi
        mesh.mesh = true
        XCTAssertEqual(LinkKind(mesh).label, "Private network")
        let hint = #"{"type":"bitrate_hint","session_id":2,"scale_percent":80}"#
        XCTAssertEqual(CoreEvent.decode(Data(hint.utf8)), .bitrateHint(sessionId: 2, scalePercent: 80))
        XCTAssertEqual(CoreEvent.decode(Data(#"{"type":"from_future"}"#.utf8)), .unknown(type: "from_future"))
        let pin = #"{"type":"pairing_pin","request_id":4,"pin":"123 456","peer_name":"PC","fingerprint_display":"AB12"}"#
        XCTAssertEqual(CoreEvent.decode(Data(pin.utf8)),
                       .pairingPin(PairingPin(requestId: 4, pin: "123 456", peerName: "PC", fingerprintDisplay: "AB12")))
    }
}
