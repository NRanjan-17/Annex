@testable import Annex
import XCTest

/// The Swift side of in-app updates: the core picks the release, the wrapper decodes it.
final class UpdateTests: XCTestCase {
    private func release(_ tag: String, pre: Bool) -> String {
        let v = tag.dropFirst()
        return """
        {"tag_name":"\(tag)","draft":false,"prerelease":\(pre),"body":"Notes","html_url":"https://example.com/\(v)",
         "assets":[{"name":"Annex-\(v)-macos-universal.dmg","browser_download_url":"https://example.com/\(v).dmg"},
                   {"name":"SHA256SUMS.txt","browser_download_url":"https://example.com/sums"},
                   {"name":"SHA256SUMS.txt.sig","browser_download_url":"https://example.com/sig"}]}
        """
    }

    func testPicksNewerReleaseAndDecodesIt() throws {
        let json = "[\(release("v99.0.0", pre: false)),\(release("v99.1.0-pre.1", pre: true))]"
        let normal = try XCTUnwrap(AnnexUpdate.pick(json: json, beta: false))
        XCTAssertEqual(normal.version, "99.0.0")
        XCTAssertEqual(normal.assetName, "Annex-99.0.0-macos-universal.dmg")
        XCTAssertEqual(normal.assetUrl, "https://example.com/99.0.0.dmg")
        XCTAssertFalse(normal.prerelease)
        XCTAssertFalse(normal.downgrade)
        let beta = try XCTUnwrap(AnnexUpdate.pick(json: json, beta: true))
        XCTAssertEqual(beta.version, "99.1.0-pre.1")
        XCTAssertTrue(beta.prerelease)
        XCTAssertEqual(Updater.headline(beta), "Annex 99.1.0-pre.1 is available. It's a pre-release.")
    }

    /// Until the user flips Beta updates, it follows what's installed; once flipped, their choice sticks.
    @MainActor
    func testBetaUpdatesFollowsInstalledVersionUntilChosen() throws {
        let suite = "annex-beta-test-\(UUID().uuidString)"
        let defaults = try XCTUnwrap(UserDefaults(suiteName: suite))
        defer { defaults.removePersistentDomain(forName: suite) }
        XCTAssertEqual(AppSettings(defaults: defaults).betaUpdates, AppSettings.runningBeta)
        let settings = AppSettings(defaults: defaults)
        settings.betaUpdates = !AppSettings.runningBeta
        XCTAssertEqual(AppSettings(defaults: defaults).betaUpdates, !AppSettings.runningBeta)
    }

    func testNothingToOfferAndBadInput() throws {
        XCTAssertNil(try AnnexUpdate.pick(json: "[]", beta: true))
        XCTAssertThrowsError(try AnnexUpdate.pick(json: "not json", beta: false))
    }

    func testVerifyRejectsUnsignedFiles() throws {
        let dir = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: dir) }
        let file = dir.appendingPathComponent("Annex-1.0.0-macos-universal.dmg")
        try Data("dmg".utf8).write(to: file)
        try Data("0000  Annex-1.0.0-macos-universal.dmg\n".utf8).write(to: dir.appendingPathComponent("SHA256SUMS.txt"))
        try Data(repeating: 0, count: 64).write(to: dir.appendingPathComponent("SHA256SUMS.txt.sig"))
        XCTAssertThrowsError(try AnnexUpdate.verify(sums: dir.appendingPathComponent("SHA256SUMS.txt"),
                                                   sig: dir.appendingPathComponent("SHA256SUMS.txt.sig"),
                                                   file: file, assetName: file.lastPathComponent))
    }
}
