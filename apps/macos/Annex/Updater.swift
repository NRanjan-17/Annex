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
    }

    @Published private(set) var state: State = .idle
    /// An automatic check found this update (the Share and Connect pages show a banner).
    @Published private(set) var announced = false

    private let settings: AppSettings
    private let session = URLSession(configuration: .ephemeral)
    private var timer: Timer?
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
                DispatchQueue.main.async { Task { await self?.check() } }
            }
            .store(in: &cancellables)
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

    /// Checking, downloading or installing: the Check Now button waits.
    var isBusy: Bool {
        switch state {
        case .checking, .downloading, .installing: return true
        default: return false
        }
    }

    func dismissBanner() {
        if case .available(let info) = state { settings.dismissedUpdateVersion = info.version }
        announced = false
    }

    nonisolated static func headline(_ info: UpdateInfo) -> String {
        "Annex \(info.version) is available." + (info.downgrade ? " It's the latest normal release." : info.prerelease ? " It's a pre-release." : "")
    }

    func check(automatic: Bool = false) async {
        switch state {
        case .checking, .downloading, .installing: return
        default: break
        }
        state = .checking
        do {
            var request = URLRequest(url: AnnexUpdate.releasesURL, timeoutInterval: 20)
            request.setValue("Annex/\(Annex.version)", forHTTPHeaderField: "User-Agent")
            request.setValue("application/vnd.github+json", forHTTPHeaderField: "Accept")
            let (data, response) = try await session.data(for: request)
            if let code = (response as? HTTPURLResponse)?.statusCode, code != 200 {
                throw AnnexError(message: "GitHub answered \(code).")
            }
            let found = try AnnexUpdate.pick(json: String(decoding: data, as: UTF8.self), beta: settings.betaUpdates)
            settings.lastUpdateCheck = Date()
            if let info = found {
                state = .available(info)
                if automatic { announced = true }
            } else {
                state = .upToDate
            }
        } catch {
            // A failed automatic check stays quiet; the next one tries again.
            state = automatic ? .idle : .failed("Couldn't check for updates. \(error.localizedDescription)")
        }
    }

    /// Asks first, then downloads, verifies and installs.
    func confirmAndInstall() {
        guard case .available(let info) = state else { return }
        let alert = NSAlert()
        alert.messageText = "Install Annex \(info.version)?"
        alert.informativeText = "Annex quits, updates and opens again. macOS asks for Screen Recording again afterwards, because this build isn't notarized yet."
        alert.addButton(withTitle: "Install")
        alert.addButton(withTitle: "Cancel")
        NSApp.activate(ignoringOtherApps: true)
        guard alert.runModal() == .alertFirstButtonReturn else { return }
        Task { await install(info) }
    }

    private func install(_ info: UpdateInfo) async {
        state = .downloading(info)
        announced = false
        let dir = FileManager.default.temporaryDirectory.appendingPathComponent("AnnexUpdate-\(UUID().uuidString)", isDirectory: true)
        do {
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

            state = .installing(info)
            let destination = Bundle.main.bundleURL
            let parent = destination.deletingLastPathComponent().path
            guard FileManager.default.isWritableFile(atPath: parent) else {
                NSWorkspace.shared.open(dmg)
                state = .manual("Drag Annex to Applications to finish updating.")
                return
            }
            let staged = try await Task.detached { try Self.stage(dmg: dmg, in: dir) }.value
            try Self.launchHelper(replacing: destination, with: staged, workDir: dir)
            NSApp.terminate(nil)
        } catch {
            try? FileManager.default.removeItem(at: dir)
            state = .failed("The update wasn't installed. \(error.localizedDescription)")
        }
    }

    private func download(_ url: String, to file: URL) async throws {
        guard let u = URL(string: url), u.scheme == "https" else { throw AnnexError(message: "The update's address isn't secure.") }
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
        defer { _ = try? run("/usr/bin/hdiutil", ["detach", mount.path, "-force"]) }
        let source = mount.appendingPathComponent("Annex.app")
        guard Bundle(url: source)?.bundleIdentifier == Bundle.main.bundleIdentifier else {
            throw AnnexError(message: "The update doesn't contain Annex.")
        }
        let staged = dir.appendingPathComponent("Annex.app")
        try run("/usr/bin/ditto", [source.path, staged.path])
        return staged
    }

    /// Starts a small script that waits for Annex to quit, swaps the app and reopens it. If the
    /// swap fails, the old app is put back and reopened.
    private static func launchHelper(replacing destination: URL, with staged: URL, workDir: URL) throws {
        let script = workDir.appendingPathComponent("install.sh")
        try """
        #!/bin/bash
        # Annex updater: replaces the app once Annex has quit, then reopens it.
        pid="$1"; dest="$2"; new="$3"; work="$4"
        while kill -0 "$pid" 2>/dev/null; do sleep 0.2; done
        old="$dest.previous"
        rm -rf "$old"
        if ! mv "$dest" "$old"; then open "$dest"; rm -rf "$work"; exit 1; fi
        if ditto "$new" "$dest"; then
          xattr -dr com.apple.quarantine "$dest" 2>/dev/null
          rm -rf "$old"
        else
          rm -rf "$dest"
          mv "$old" "$dest"
        fi
        open "$dest"
        rm -rf "$work"
        """.write(to: script, atomically: true, encoding: .utf8)
        // Detached from this process, so it keeps running after Annex quits.
        let p = Process()
        p.executableURL = URL(fileURLWithPath: "/bin/sh")
        p.arguments = ["-c", "nohup /bin/bash \"$0\" \"$@\" >/dev/null 2>&1 &", script.path,
                       String(ProcessInfo.processInfo.processIdentifier), destination.path, staged.path, workDir.path]
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
