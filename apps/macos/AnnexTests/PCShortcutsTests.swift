import XCTest
@testable import Annex

final class PCShortcutsTests: XCTestCase {
    private func run(_ presses: [(UInt16, Bool)]) -> [String] {
        var m = PCShortcuts()
        return presses.flatMap { m.translate($0.0, down: $0.1) }.map { "\(String($0.code, radix: 16))\($0.down ? "↓" : "↑")" }
    }

    func testCommandCIsControlC() {
        // ⌘ (0x37) C (0x08) → Ctrl (0x3b) C.
        XCTAssertEqual(run([(0x37, true), (0x08, true), (0x08, false), (0x37, false)]), ["3b↓", "8↓", "8↑", "3b↑"])
    }

    func testCommandTabIsAltTabUntilCommandIsReleased() {
        XCTAssertEqual(run([(0x37, true), (0x30, true), (0x30, false), (0x30, true), (0x30, false), (0x37, false)]),
                       ["3b↓", "3b↑", "3a↓", "30↓", "30↑", "30↓", "30↑", "3a↑"])
    }

    func testCommandSpaceTapsTheWindowsKey() {
        XCTAssertEqual(run([(0x37, true), (0x31, true), (0x31, false), (0x37, false)]), ["3b↓", "3b↑", "37↓", "37↑", "31↑"])
    }

    func testCommandQIsAltF4() {
        XCTAssertEqual(run([(0x37, true), (0x0C, true), (0x0C, false), (0x37, false)]),
                       ["3b↓", "3b↑", "3a↓", "76↓", "76↑", "3a↑", "c↑"])
    }

    func testControlIsTheWindowsKeyAndLettersPassThrough() {
        XCTAssertEqual(run([(0x3B, true), (0x7B, true), (0x7B, false), (0x3B, false), (0x00, true), (0x00, false)]),
                       ["37↓", "7b↓", "7b↑", "37↑", "0↓", "0↑"])
    }

    func testRightCommandIsRightControl() {
        XCTAssertEqual(run([(0x36, true), (0x09, true), (0x09, false), (0x36, false)]), ["3e↓", "9↓", "9↑", "3e↑"])
    }
}
