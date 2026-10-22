@testable import Annex
import XCTest

/// Runs a real host ↔ viewer session through the Swift wrappers and the Rust core.
final class CoreBridgeTests: XCTestCase {
    func testLoopbackSessionThroughSwiftWrappers() throws {
        let hostCore = try CoreClient(ephemeralName: "Swift Host")
        let viewerCore = try CoreClient(ephemeralName: "Swift Viewer")
        XCTAssertEqual(hostCore.fingerprint.count, 64)

        let started = expectation(description: "host session started")
        let keyframeAsked = expectation(description: "keyframe requested")
        keyframeAsked.assertForOverFulfill = false
        let host = try HostHandle(core: hostCore, port: 0, width: 1280, height: 720, fps: 60, advertise: false) { e in
            switch e {
            case .trustRequest(let r): hostCore.respondTrust(requestID: r.requestId, accept: true)
            case .sessionStarted: started.fulfill()
            case .keyframeRequested: keyframeAsked.fulfill()
            default: break
            }
        }

        let gotFrame = expectation(description: "frame received")
        let gotConfig = expectation(description: "stream config")
        var received = Data()
        let viewer = try ViewerHandle(
            core: viewerCore, address: "127.0.0.1:\(host.port)", expectedFingerprint: hostCore.fingerprint,
            display: DisplayInfo(widthPx: 2560, heightPx: 1600, scalePercent: 200),
            onEvent: { e in
                switch e {
                case .trustRequest(let r): viewerCore.respondTrust(requestID: r.requestId, accept: true)
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
        wait(for: [started, gotConfig, keyframeAsked], timeout: 10)

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
