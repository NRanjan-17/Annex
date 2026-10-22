@testable import Annex
import AppKit
import CoreGraphics
import XCTest

/// Extend mode end to end: a viewer asks for Extend, the host creates a real virtual display
/// with the viewer's resolution, and removes it when the viewer disconnects.
@MainActor
final class ExtendTests: XCTestCase {
    private func annexDisplays() -> [CGDirectDisplayID] {
        var ids = [CGDirectDisplayID](repeating: 0, count: 32)
        var count: UInt32 = 0
        CGGetOnlineDisplayList(32, &ids, &count)
        return ids.prefix(Int(count)).filter { CGDisplayVendorNumber($0) == ExtendSession.vendorID }
    }

    private func waitUntil(_ timeout: TimeInterval = 10, _ condition: () -> Bool) -> Bool {
        let deadline = Date().addingTimeInterval(timeout)
        while Date() < deadline {
            if condition() { return true }
            RunLoop.main.run(until: Date().addingTimeInterval(0.05))
        }
        return condition()
    }

    func testExtendCreatesAndRemovesVirtualDisplay() throws {
        try XCTSkipUnless(AnnexVirtualDisplay.isSupported(), "CGVirtualDisplay unavailable on this macOS")
        let display = try XCTUnwrap(Displays.all().first)
        ExtendSession.captureEnabled = false
        defer { ExtendSession.captureEnabled = true }
        XCTAssertTrue(annexDisplays().isEmpty, "a stale Annex display is present")

        let hostCore = try CoreClient(ephemeralName: "Extend Host")
        let viewerCore = try CoreClient(ephemeralName: "Extend Viewer")
        let pairing = LockedBox<(pin: String?, request: UInt64?, done: Bool)>()
        pairing.value = (nil, nil, false)
        let note: (String?, UInt64?) -> Void = { pin, request in
            let ready: (String, UInt64)? = pairing.update { v in
                if let pin { v!.pin = pin }
                if let request { v!.request = request }
                guard !v!.done, let p = v!.pin, let r = v!.request else { return nil }
                v!.done = true
                return (p, r)
            }
            if let (pin, req) = ready { viewerCore.submitPin(requestID: req, pin: pin) }
        }

        let controller = HostController(core: hostCore, display: display, preferHardware: true)
        controller.onEvent = { e in if case .pairingPin(let p) = e { note(p.pin, nil) } }
        try controller.start()
        let port = try XCTUnwrap(controller.status.listeningPort)

        var ended = false
        let latestStream = LockedBox<StreamConfig>()
        var viewer: ViewerHandle? = try ViewerHandle(
            core: viewerCore, address: "127.0.0.1:\(port)", expectedFingerprint: nil,
            display: DisplayInfo(widthPx: 2560, heightPx: 1600, widthMm: 330, heightMm: 206, refreshHz: 60, scalePercent: 200),
            extend: true,
            onEvent: { e in
                if case .pairingPinRequired(let r) = e { note(nil, r.requestId) }
                if case .sessionEnded = e { DispatchQueue.main.async { ended = true } }
                if case .streamConfig(let st) = e { latestStream.value = st }
            },
            onFrame: { _, _, _, _ in })

        XCTAssertTrue(waitUntil { !controller.status.extendViewers.isEmpty }, "extend session did not start")
        XCTAssertEqual(controller.status.extendViewers, ["Extend Viewer"])
        XCTAssertTrue(waitUntil { self.annexDisplays().count == 1 }, "virtual display did not appear")
        let id = annexDisplays()[0]
        let mode = try XCTUnwrap(CGDisplayCopyDisplayMode(id))
        XCTAssertEqual(mode.pixelWidth, 2560)
        XCTAssertEqual(mode.pixelHeight, 1600)
        XCTAssertEqual(mode.width, 1280, "Retina viewer → 2x HiDPI default mode")
        XCTAssertEqual(CGDisplayModelNumber(id), ExtendSession.productID)
        XCTAssertEqual(CGDisplaySerialNumber(id), ExtendSession.serial(for: viewerCore.fingerprint))
        XCTAssertTrue(waitUntil { latestStream.value?.width == 2560 }, "viewer should start at its native size")
        // The name System Settings → Displays shows.
        let screen = NSScreen.screens.first { ($0.deviceDescription[NSDeviceDescriptionKey("NSScreenNumber")] as? NSNumber)?.uint32Value == id }
        XCTAssertEqual(screen?.localizedName, "Annex Display")

        // Acceptance: change the resolution like System Settings would; the viewer must follow
        // within a second. (Session-only change, nothing is persisted.)
        let options = [kCGDisplayShowDuplicateLowResolutionModes as String: true] as CFDictionary
        let modes = (CGDisplayCopyAllDisplayModes(id, options) as? [CGDisplayMode]) ?? []
        let target = try XCTUnwrap(modes.first { $0.pixelWidth == 1920 && $0.pixelHeight == 1200 }, "no 1920x1200 px mode offered")
        var config: CGDisplayConfigRef?
        XCTAssertEqual(CGBeginDisplayConfiguration(&config), .success)
        CGConfigureDisplayWithDisplayMode(config, id, target, nil)
        XCTAssertEqual(CGCompleteDisplayConfiguration(config, .forSession), .success)
        let changedAt = Date()
        XCTAssertTrue(waitUntil(3) { latestStream.value?.width == 1920 && latestStream.value?.height == 1200 },
                      "viewer was not told about the new resolution")
        XCTAssertLessThan(Date().timeIntervalSince(changedAt), 1.0, "viewer must update within 1 s")

        viewer?.disconnect()
        viewer = nil
        XCTAssertTrue(waitUntil { self.annexDisplays().isEmpty }, "virtual display was not removed")
        XCTAssertTrue(waitUntil { controller.status.extendViewers.isEmpty })
        controller.stop()
        _ = ended
    }
}
