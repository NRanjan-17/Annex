@testable import Annex
import XCTest

/// Runs a real host ↔ viewer session through the Swift wrappers and the Rust core.
final class CoreBridgeTests: XCTestCase {
    func testLoopbackSessionThroughSwiftWrappers() throws {
        let hostCore = try CoreClient(ephemeralName: "Swift Host")
        let viewerCore = try CoreClient(ephemeralName: "Swift Viewer")
        XCTAssertEqual(hostCore.fingerprint.count, 64)

        // PIN pairing: the host shows a PIN, the viewer's "user" types it.
        // Whichever of the two events arrives second submits the PIN, exactly once.
        let pairing = LockedBox<(pin: String?, request: UInt64?, submitted: Bool)>()
        pairing.value = (nil, nil, false)
        let note: (String?, UInt64?) -> Void = { pin, request in
            let ready: (String, UInt64)? = pairing.update { v in
                if let pin { v!.pin = pin }
                if let request { v!.request = request }
                guard !v!.submitted, let p = v!.pin, let r = v!.request else { return nil }
                v!.submitted = true
                return (p, r)
            }
            if let (pin, req) = ready { XCTAssertTrue(viewerCore.submitPin(requestID: req, pin: pin)) }
        }
        let paired = expectation(description: "paired")
        paired.expectedFulfillmentCount = 2
        let started = expectation(description: "host session started")
        let keyframeAsked = expectation(description: "keyframe requested")
        keyframeAsked.assertForOverFulfill = false
        let host = try HostHandle(core: hostCore, port: 0, width: 1280, height: 720, fps: 60, advertise: false, extend: true) { e in
            switch e {
            case .pairingPin(let p):
                note(p.pin, nil)
            case .pairingFinished(let f):
                XCTAssertTrue(f.success)
                paired.fulfill()
            case .sessionStarted: started.fulfill()
            case .keyframeRequested: keyframeAsked.fulfill()
            default: break
            }
        }

        let gotFrame = expectation(description: "frame received")
        let gotConfig = expectation(description: "stream config")
        var received = Data()
        let viewer = try ViewerHandle(
            core: viewerCore, address: "127.0.0.1:\(host.port)", expectedFingerprint: nil,
            display: DisplayInfo(widthPx: 2560, heightPx: 1600, scalePercent: 200),
            onEvent: { e in
                switch e {
                case .pairingPinRequired(let r):
                    XCTAssertEqual(r.peerName, "Swift Host")
                    note(nil, r.requestId)
                case .pairingFinished(let f):
                    XCTAssertTrue(f.success)
                    paired.fulfill()
                case .streamConfig(let s):
                    XCTAssertEqual(s, StreamConfig(codec: "h264", width: 1280, height: 720, fps: 60))
                    gotConfig.fulfill()
                default: break
                }
            },
            onFrame: { buf, _, _, key in
                XCTAssertTrue(key)
                received = Data(buf)
                gotFrame.fulfill()
            })
        wait(for: [paired, started, gotConfig, keyframeAsked], timeout: 10)

        let au = Data([0, 0, 0, 1, 0x65, 0xAA, 0xBB])
        XCTAssertEqual(host.submit(au, captureMicros: Annex.nowMicros(), keyframe: true), .sent)
        wait(for: [gotFrame], timeout: 10)
        XCTAssertEqual(received, au)

        viewer.disconnect()
        host.stop()
        XCTAssertEqual(hostCore.trustedPeers.map(\.name), ["Swift Viewer"])
        XCTAssertTrue(try hostCore.untrust(viewerCore.fingerprint))
        XCTAssertTrue(hostCore.trustedPeers.isEmpty)
    }
}
