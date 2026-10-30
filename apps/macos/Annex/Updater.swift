import AppKit
import Combine
import Foundation

/// In-app updates from this project's GitHub releases. Checks once a day (when allowed), downloads
/// the disk image, installs it only if its signed checksum verifies, then replaces the app and
/// reopens it. Nothing but the releases request and the downloads leaves this Mac.
@MainActor
final class Updater: ObservableObject {
    enum State: Equatable {
        case idle
        case checking
        case upToDate
        case available(UpdateInfo)
        case downloading(UpdateInfo)
        case installing(UpdateInfo)
        case failed(String)
        /// The update is verified but must be finished by hand (the app's folder isn't writable).
        case manual(String)
        /// No one at this screen: downloaded and verified, it installs once no one is connected.
        case ready(UpdateInfo)
    }

    /// No one at this screen is on: updates install without asking (set by AppModel).
    var unattended: () -> Bool = { false }
    /// Someone is viewing this Mac, or it views another (set by AppModel).
    var busy: () -> Bool = { false }
    /// Just before Annex quits to update unattended (AppModel remembers to share again).
    var willQuitToUpdate: () -> Void = {}

    /// No one connected for this long before an unattended update installs, so a viewer switching
    /// from mirror to extend isn't dropped in between.
    private static let idleBeforeInstall: TimeInterval = 60
    /// The new version must be running and sharing this soon, or the old one comes back.
    static let confirmWithin = 90
    private var staged: (info: UpdateInfo, app: URL, dir: URL)?
    private var idleSince: Date?
    private var installTimer: Timer?

    @Published private(set) var state: State = .idle {
        didSet { if state != oldValue { Annex.log(.debug, "[update] \(Self.describe(state))") } }
    }
    /// An automatic check found this update (the Share and Connect pages show a banner).
    @Published private(set) var announced = false

    private let settings: AppSettings
    private let session = URLSession(configuration: .ephemeral)
    private var timer: Timer?
    /// Beta updates changed while a check was running: check again when it ends.
    private var recheck = false
    private var cancellables: Set<AnyCancellable> = []

    init(settings: AppSettings) {
        self.settings = settings
    }

    /// Starts the daily check (if allowed) and re-checks when Beta updates changes.
    func startAutomaticChecks() {
        settings.$checkForUpdates.removeDuplicates()
            .sink { [weak self] on in self?.schedule(on) }
            .store(in: &cancellables)
        settings.$betaUpdates.dropFirst().removeDuplicates()
            .sink { [weak self] _ in
                // Read the new value after the property has changed.
                DispatchQueue.main.async { self?.betaChanged() }
            }
            .store(in: &cancellables)
    }

    /// The old offer was for the other setting: drop it now, then look again with the new one.
    /// A state for the log (without release notes).
    private static func describe(_ state: State) -> String {
        switch state {
        case .idle: "idle"
        case .checking: "checking"
        case .upToDate: "up to date (\(Annex.version))"
        case .available(let i): "\(i.version) available\(i.prerelease ? " (pre-release)" : "")\(i.downgrade ? " (downgrade)" : "")"
        case .downloading(let i): "downloading \(i.version)"
        case .installing(let i): "installing \(i.version)"
        case .failed(let why): "failed: \(why)"
        case .manual(let why): "needs finishing by hand: \(why)"
        case .ready(let i): "\(i.version) downloaded, installs when no one is connected"
        }
    }

    private func betaChanged() {
        if case .available = state { state = .idle }
        if case .ready = state { discardStaged(); state = .idle }
        announced = false
        Task { await check() }
    }

    private func schedule(_ on: Bool) {
        timer?.invalidate()
        timer = nil
        guard on else { return }
        DispatchQueue.main.asyncAfter(deadline: .now() + 10) { [weak self] in
            guard let self, self.settings.checkForUpdates else { return }
            Task { await self.check(automatic: true) }
        }
        timer = Timer.scheduledTimer(withTimeInterval: 24 * 3600, repeats: true) { [weak self] _ in
            Task { @MainActor in await self?.check(automatic: true) }
        }
    }

    /// Whether the banner should show for the current update.
    var bannerUpdate: UpdateInfo? {
        guard announced, case .available(let info) = state, info.version != settings.dismissedUpdateVersion else { return nil }
        return info
    }

    /// The update the Install button installs: offered, or downloaded and waiting.
    var offered: UpdateInfo? {
        switch state {
        case .available(let info), .ready(let info): info
        default: nil
        }
    }

    /// Checking, downloading or installing: the Check Now button waits.
    var isBusy: Bool {
        switch state {
        case .checking, .downloading, .installing: return true
        default: return false
        }
    }

    func dismissBanner() {
        if case .available(let info) = state {
            Annex.log(.info, "[update] Not Now pressed for \(info.version)")
            settings.dismissedUpdateVersion = info.version
        }
        announced = false
    }

    nonisolated static func headline(_ info: UpdateInfo) -> String {
        "Annex \(info.version) is available." + (info.downgrade ? " It's the latest normal release." : info.prerelease ? " It's a pre-release." : "")
    }

    func check(automatic: Bool = false) async {
        switch state {
        case .checking:
            if !automatic { recheck = true }
            return
        case .downloading, .installing: return
        default: break
        }
        let waiting = staged
        state = .checking
        let beta = settings.betaUpdates
        Annex.log(.info, "[update] checking \(AnnexUpdate.releasesURL) (\(automatic ? "daily" : "asked"), beta \(beta))")
        do {
            var request = URLRequest(url: AnnexUpdate.releasesURL, timeoutInterval: 20)
            request.setValue("Annex/\(Annex.version)", forHTTPHeaderField: "User-Agent")
            request.setValue("application/vnd.github+json", forHTTPHeaderField: "Accept")
            let (data, response) = try await session.data(for: request)
            if let code = (response as? HTTPURLResponse)?.statusCode, code != 200 {
                throw AnnexError(message: "GitHub answered \(code).")
            }
            var found = try AnnexUpdate.pick(json: String(decoding: data, as: UTF8.self), beta: beta)
            settings.lastUpdateCheck = Date()
            // Going back to an older normal release is only offered when asked for (Beta updates
            // turned off, or Check Now), never by the daily check.
            if automatic, found?.downgrade == true {
                Annex.log(.info, "[update] \(found?.version ?? "") is a downgrade: not offered by the daily check")
                found = nil
            }
            if let info = found, let waiting, waiting.info.version == info.version {
                state = .ready(waiting.info)
            } else if let info = found {
                discardStaged()
                state = .available(info)
                if automatic { announced = true }
                if automatic, unattended() { Task { await prepareUnattended(info) } }
            } else {
                state = .upToDate
            }
        } catch {
            Annex.log(.warn, "[update] check failed: \(error.localizedDescription)")
            // A failed automatic check stays quiet; the next one tries again.
            if let waiting {
                state = .ready(waiting.info)
            } else {
                state = automatic ? .idle : .failed("Couldn't check for updates. \(error.localizedDescription)")
            }
        }
        // Beta updates was flipped mid-check: that answer was for the old setting.
        if recheck || beta != settings.betaUpdates {
            recheck = false
            if case .available = state { state = .idle }
            await check()
        }
    }

    /// Asks first, then downloads, verifies and installs.
    func confirmAndInstall() {
        if case .ready(let info) = state {
            confirm(info)
            return
        }
        guard case .available(let offered) = state else { return }
        Task {
            // The offer can be hours old and releases come quickly: look again so this installs
            // the newest release, not the next one after this version. If GitHub can't be
            // reached, the earlier offer stands.
            await check()
            if case .failed = state { state = .available(offered) }
            guard case .available(let info) = state else { return }
            confirm(info)
        }
    }

    private func confirm(_ info: UpdateInfo) {
        let alert = NSAlert()
        alert.messageText = "Install Annex \(info.version)?"
        alert.informativeText = "Annex quits, updates and opens again. Its permissions stay as they are."
        alert.addButton(withTitle: "Install")
        alert.addButton(withTitle: "Cancel")
        NSApp.activate(ignoringOtherApps: true)
        let confirmed = alert.runModal() == .alertFirstButtonReturn
        Annex.log(.info, "[update] the user chose \(confirmed ? "Install" : "Cancel") for \(info.version)")
        guard confirmed else { return }
        Task { await install(info) }
    }

    private func install(_ info: UpdateInfo) async {
        if let waiting = staged, waiting.info.version == info.version {
            state = .installing(info)
            return replace(with: waiting.app, workDir: waiting.dir, unattended: nil)
        }
        discardStaged()
        state = .downloading(info)
        announced = false
        let dir = FileManager.default.temporaryDirectory.appendingPathComponent("AnnexUpdate-\(UUID().uuidString)", isDirectory: true)
        do {
            let dmg = try await fetch(info, into: dir)
            state = .installing(info)
            let parent = Bundle.main.bundleURL.deletingLastPathComponent().path
            guard FileManager.default.isWritableFile(atPath: parent) else {
                Annex.log(.warn, "[update] \(parent) isn't writable: opening the disk image to finish by hand")
                NSWorkspace.shared.open(dmg)
                state = .manual("Drag Annex to Applications to finish updating.")
                return
            }
            let app = try await Task.detached { try Self.stage(dmg: dmg, in: dir) }.value
            replace(with: app, workDir: dir, unattended: nil)
        } catch {
            Annex.log(.error, "[update] install failed: \(error.localizedDescription)")
            do {
                try FileManager.default.removeItem(at: dir)
            } catch {
                Annex.log(.debug, "[update] couldn't remove \(dir.path): \(error.localizedDescription)")
            }
            state = .failed("The update wasn't installed. \(error.localizedDescription)")
        }
    }

    /// Downloads the update into `dir` and checks its signature; returns the disk image.
    private func fetch(_ info: UpdateInfo, into dir: URL) async throws -> URL {
        try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        // Only the file name from the release; never a path.
        let name = URL(fileURLWithPath: info.assetName).lastPathComponent
        guard !name.isEmpty, name != "..", name == info.assetName else { throw AnnexError(message: "The update has an unexpected file name.") }
        let dmg = dir.appendingPathComponent(name)
        let sums = dir.appendingPathComponent("SHA256SUMS.txt")
        let sig = dir.appendingPathComponent("SHA256SUMS.txt.sig")
        try await download(info.assetUrl, to: dmg)
        try await download(info.sumsUrl, to: sums)
        try await download(info.sigUrl, to: sig)
        try AnnexUpdate.verify(sums: sums, sig: sig, file: dmg, assetName: info.assetName)
        Annex.log(.info, "[update] \(name) downloaded and its signature verified")
        return dmg
    }

    /// Starts the helper that swaps the app, then quits. `unattended`: the version the helper waits
    /// for to say it's running (see `confirmUpdate`), or nil when someone pressed Install.
    private func replace(with app: URL, workDir: URL, unattended version: String?) {
        let destination = Bundle.main.bundleURL
        do {
            try Self.launchHelper(replacing: destination, with: app, workDir: workDir, confirming: version)
        } catch {
            Annex.log(.error, "[update] couldn't start the installer: \(error.localizedDescription)")
            state = .failed("The update wasn't installed. \(error.localizedDescription)")
            return
        }
        Annex.log(.info, "[update] replacing \(destination.path) after quitting")
        NSApp.terminate(nil)
    }

    // MARK: No one at this screen

    /// Downloads, verifies and unpacks the update now; it installs once no one is connected.
    private func prepareUnattended(_ info: UpdateInfo) async {
        if Self.failedVersion == info.version {
            return Annex.log(.warn, "[update] \(info.version) didn't start here before: not installing it unattended")
        }
        guard !info.downgrade else { return }
        let parent = Bundle.main.bundleURL.deletingLastPathComponent().path
        guard FileManager.default.isWritableFile(atPath: parent) else {
            return Annex.log(.warn, "[update] \(parent) isn't writable: \(info.version) can't install unattended")
        }
        state = .downloading(info)
        Annex.log(.info, "[update] No one at this screen: downloading \(info.version) to install once no one is connected")
        let dir = FileManager.default.temporaryDirectory.appendingPathComponent("AnnexUpdate-\(UUID().uuidString)", isDirectory: true)
        do {
            let dmg = try await fetch(info, into: dir)
            let app = try await Task.detached { try Self.stage(dmg: dmg, in: dir) }.value
            staged = (info, app, dir)
            state = .ready(info)
            sessionsChanged()
            installTimer?.invalidate()
            installTimer = Timer.scheduledTimer(withTimeInterval: 15, repeats: true) { [weak self] _ in
                Task { @MainActor in self?.installIfIdle() }
            }
        } catch {
            Annex.log(.warn, "[update] couldn't prepare \(info.version): \(error.localizedDescription)")
            try? FileManager.default.removeItem(at: dir)
            state = .available(info)
        }
    }

    /// AppModel calls this when a session starts or ends.
    func sessionsChanged() {
        idleSince = busy() ? nil : (idleSince ?? Date())
    }

    private func installIfIdle() {
        guard let waiting = staged, case .ready = state else { return }
        guard unattended() else { return }
        sessionsChanged()
        guard let since = idleSince, Date().timeIntervalSince(since) >= Self.idleBeforeInstall else { return }
        installTimer?.invalidate()
        installTimer = nil
        Annex.log(.info, "[update] no one connected for \(Int(Self.idleBeforeInstall)) s: installing \(waiting.info.version) unattended")
        state = .installing(waiting.info)
        willQuitToUpdate()
        UserDefaults.standard.set(waiting.info.version, forKey: Self.confirmKey)
        replace(with: waiting.app, workDir: waiting.dir, unattended: waiting.info.version)
    }

    private func discardStaged() {
        installTimer?.invalidate()
        installTimer = nil
        if let dir = staged?.dir { try? FileManager.default.removeItem(at: dir) }
        staged = nil
    }

    /// The version an unattended update installed, until it says it's running (`confirmUpdate`).
    private static let confirmKey = "unattendedUpdateToConfirm"
    /// Written by the helper when it put the old version back: never installed unattended again.
    private static let failedKey = "unattendedUpdateFailed"
    private static var failedVersion: String? { UserDefaults.standard.string(forKey: failedKey) }

    /// The file the helper waits for after an unattended update.
    private static var confirmFile: URL {
        FileManager.default.homeDirectoryForCurrentUser.appendingPathComponent(".annex-update-ok")
    }

    /// Called once this Mac is up (sharing, if it should be): tells a waiting helper the new
    /// version works. A version that came back after a failed update logs that instead.
    static func confirmUpdate() {
        if let failed = failedVersion, failed != Annex.version, UserDefaults.standard.string(forKey: confirmKey) == failed {
            Annex.log(.warn, "[update] \(failed) didn't start sharing in \(confirmWithin) s: back on \(Annex.version)")
            UserDefaults.standard.removeObject(forKey: confirmKey)
        }
        guard let version = UserDefaults.standard.string(forKey: confirmKey), version == Annex.version else { return }
        UserDefaults.standard.removeObject(forKey: confirmKey)
        do {
            try Data(version.utf8).write(to: confirmFile)
            Annex.log(.info, "[update] updated to \(version) unattended, and it runs")
        } catch {
            Annex.log(.warn, "[update] couldn't confirm the update: \(error.localizedDescription)")
        }
    }

    private func download(_ url: String, to file: URL) async throws {
        guard let u = URL(string: url), u.scheme == "https" else { throw AnnexError(message: "The update's address isn't secure.") }
        Annex.log(.debug, "[update] downloading \(u)")
        var request = URLRequest(url: u, timeoutInterval: 60)
        request.setValue("Annex/\(Annex.version)", forHTTPHeaderField: "User-Agent")
        let (temp, response) = try await session.download(for: request)
        if let code = (response as? HTTPURLResponse)?.statusCode, code != 200 {
            throw AnnexError(message: "The download failed (\(code)).")
        }
        try FileManager.default.moveItem(at: temp, to: file)
    }

    /// Mounts the verified disk image and copies the app out of it.
    nonisolated private static func stage(dmg: URL, in dir: URL) throws -> URL {
        let mount = dir.appendingPathComponent("mnt", isDirectory: true)
        try run("/usr/bin/hdiutil", ["attach", "-nobrowse", "-readonly", "-noautoopen", "-mountpoint", mount.path, dmg.path])
        defer {
            do {
                try run("/usr/bin/hdiutil", ["detach", mount.path, "-force"])
            } catch {
                Annex.log(.warn, "[update] couldn't unmount the update: \(error.localizedDescription)")
            }
        }
        let source = mount.appendingPathComponent("Annex.app")
        guard Bundle(url: source)?.bundleIdentifier == Bundle.main.bundleIdentifier else {
            throw AnnexError(message: "The update doesn't contain Annex.")
        }
        let staged = dir.appendingPathComponent("Annex.app")
        try run("/usr/bin/ditto", [source.path, staged.path])
        return staged
    }

    /// Starts a small script that waits for Annex to quit, swaps the app and reopens it. If the
    /// swap fails, the old app is put back and reopened. Unattended (`confirming` a version), the
    /// old app is kept until the new one writes the confirm file; if it doesn't within
    /// `confirmWithin` seconds, the new one is closed, the old one put back and reopened, and that
    /// version is never installed unattended again.
    private static func launchHelper(replacing destination: URL, with staged: URL, workDir: URL, confirming version: String?) throws {
        let script = workDir.appendingPathComponent("install.sh")
        try """
        #!/bin/bash
        # Annex updater: replaces the app once Annex has quit, then reopens it.
        pid="$1"; dest="$2"; new="$3"; work="$4"; version="$5"; ok="$6"; domain="$7"; wait="$8"
        while kill -0 "$pid" 2>/dev/null; do sleep 0.2; done
        old="$dest.previous"
        rm -rf "$old"
        if ! mv "$dest" "$old"; then open "$dest"; rm -rf "$work"; exit 1; fi
        if ditto "$new" "$dest"; then
          xattr -dr com.apple.quarantine "$dest" 2>/dev/null
        else
          rm -rf "$dest"
          mv "$old" "$dest"
          open "$dest"
          rm -rf "$work"
          exit 1
        fi
        if [ -z "$version" ]; then
          rm -rf "$old"
          open "$dest"
          rm -rf "$work"
          exit 0
        fi
        rm -f "$ok"
        open "$dest"
        for _ in $(seq 1 "$wait"); do
          if [ "$(cat "$ok" 2>/dev/null)" = "$version" ]; then rm -f "$ok"; rm -rf "$old" "$work"; exit 0; fi
          sleep 1
        done
        # The new version isn't running and sharing: put the old one back.
        pkill -f "$dest/Contents/MacOS/" 2>/dev/null
        sleep 2
        rm -rf "$dest"
        mv "$old" "$dest"
        defaults write "$domain" unattendedUpdateFailed -string "$version"
        defaults write "$domain" shareAfterUpdate -bool true
        open "$dest"
        rm -rf "$work"
        """.write(to: script, atomically: true, encoding: .utf8)
        // Detached from this process, so it keeps running after Annex quits.
        let p = Process()
        p.executableURL = URL(fileURLWithPath: "/bin/sh")
        p.arguments = ["-c", "nohup /bin/bash \"$0\" \"$@\" >/dev/null 2>&1 &", script.path,
                       String(ProcessInfo.processInfo.processIdentifier), destination.path, staged.path, workDir.path,
                       version ?? "", confirmFile.path, Bundle.main.bundleIdentifier ?? "", String(confirmWithin)]
        try p.run()
        p.waitUntilExit()
    }

    @discardableResult
    nonisolated private static func run(_ tool: String, _ args: [String]) throws -> Int32 {
        let p = Process()
        p.executableURL = URL(fileURLWithPath: tool)
        p.arguments = args
        p.standardOutput = FileHandle.nullDevice
        p.standardError = FileHandle.nullDevice
        try p.run()
        p.waitUntilExit()
        guard p.terminationStatus == 0 else {
            throw AnnexError(message: "\(URL(fileURLWithPath: tool).lastPathComponent) failed (\(p.terminationStatus)).")
        }
        return p.terminationStatus
    }
}
