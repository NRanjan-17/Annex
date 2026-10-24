import Combine
import Foundation
import ServiceManagement

/// User preferences, persisted in UserDefaults (removed by "Uninstall Annex").
@MainActor
final class AppSettings: ObservableObject {
    private let defaults: UserDefaults

    @Published var preferHardwareEncoder: Bool { didSet { defaults.set(preferHardwareEncoder, forKey: Keys.hardware) } }
    @Published var startSharingAtLaunch: Bool { didSet { defaults.set(startSharingAtLaunch, forKey: Keys.autoShare) } }
    /// Viewer: ask hosts to let this Mac's mouse and keyboard control them. On by default: the
    /// sharing computer still decides per device, so asking is harmless.
    @Published var controlWhenViewing: Bool { didSet { defaults.set(controlWhenViewing, forKey: Keys.control) } }
    @Published var openFullScreen: Bool { didSet { defaults.set(openFullScreen, forKey: Keys.fullScreen) } }
    /// Viewer: default to using the other computer as an extra display.
    @Published var extendByDefault: Bool { didSet { defaults.set(extendByDefault, forKey: Keys.extend) } }
    /// Viewer: bitrate ceiling asked of hosts. 0 best, 1 balanced, 2 fast (Wi-Fi).
    @Published var quality: Int { didSet { defaults.set(quality, forKey: Keys.quality) } }
    /// Show identity codes, addresses, stream sizes and latency in the UI. Off by default.
    @Published var showTechnicalDetails: Bool { didSet { defaults.set(showTechnicalDetails, forKey: Keys.technical) } }
    /// Write annex.log for bug reports. Off by default.
    @Published var saveLog: Bool { didSet { defaults.set(saveLog, forKey: Keys.log); Annex.setLogEnabled(saveLog) } }
    /// Look for a new version on GitHub once a day.
    @Published var checkForUpdates: Bool { didSet { defaults.set(checkForUpdates, forKey: Keys.updates) } }
    /// Offer pre-releases too. Turning it off offers the newest normal release.
    @Published var betaUpdates: Bool { didSet { defaults.set(betaUpdates, forKey: Keys.beta) } }
    /// The update the user said "Not now" to (its banner stays hidden; a newer one shows again).
    @Published var dismissedUpdateVersion: String? { didSet { defaults.set(dismissedUpdateVersion, forKey: Keys.dismissedUpdate) } }
    /// When the last update check finished (shown on the About page).
    @Published var lastUpdateCheck: Date? { didSet { defaults.set(lastUpdateCheck, forKey: Keys.lastCheck) } }
    /// Host: the size of text and windows on a screen extending this Mac. 0 = sharpest (100 %, or
    /// 200 % on a high-resolution screen); otherwise a scale step, 100–200.
    @Published var extendScale: Int { didSet { defaults.set(extendScale, forKey: Keys.extendScale) } }
    /// Mac ↔ Mac with no router, over peer-to-peer Wi-Fi (what AirDrop uses). On by default.
    @Published var peerToPeerWiFi: Bool { didSet { defaults.set(peerToPeerWiFi, forKey: Keys.peerToPeer) } }
    /// The name other devices see; nil = this Mac's name from System Settings.
    @Published var customDeviceName: String? { didSet { defaults.set(customDeviceName, forKey: Keys.name) } }
    @Published private(set) var launchAtLogin = false
    @Published private(set) var launchAtLoginError: String?

    private enum Keys {
        static let hardware = "preferHardwareEncoder"
        static let autoShare = "startSharingAtLaunch"
        static let control = "useMouseAndKeyboard"
        static let fullScreen = "openFullScreen"
        static let extend = "extendByDefault"
        static let name = "deviceName"
        static let quality = "quality"
        static let log = "saveLog"
        static let technical = "showTechnicalDetails"
        static let updates = "checkForUpdates"
        static let beta = "betaUpdates"
        static let dismissedUpdate = "dismissedUpdateVersion"
        static let lastCheck = "lastUpdateCheck"
        static let peerToPeer = "peerToPeerWiFi"
        static let extendScale = "extendScale"
    }

    /// This copy is a pre-release (`0.2.0-pre.1`).
    static var runningBeta: Bool { Annex.version.contains("-") }

    init(defaults: UserDefaults = .standard) {
        self.defaults = defaults
        defaults.register(defaults: [Keys.hardware: true, Keys.fullScreen: true, Keys.control: true, Keys.updates: true,
                                       Keys.peerToPeer: true])
        preferHardwareEncoder = defaults.bool(forKey: Keys.hardware)
        startSharingAtLaunch = defaults.bool(forKey: Keys.autoShare)
        controlWhenViewing = defaults.bool(forKey: Keys.control)
        openFullScreen = defaults.bool(forKey: Keys.fullScreen)
        extendByDefault = defaults.bool(forKey: Keys.extend)
        customDeviceName = defaults.string(forKey: Keys.name)
        quality = defaults.integer(forKey: Keys.quality)
        saveLog = defaults.bool(forKey: Keys.log)
        showTechnicalDetails = defaults.bool(forKey: Keys.technical)
        checkForUpdates = defaults.bool(forKey: Keys.updates)
        // Saved only when the user flips it. Until then it follows what's installed: someone who
        // installed a beta by hand gets betas, not an offer to go back to the last normal release.
        betaUpdates = defaults.object(forKey: Keys.beta) as? Bool ?? Self.runningBeta
        dismissedUpdateVersion = defaults.string(forKey: Keys.dismissedUpdate)
        lastUpdateCheck = defaults.object(forKey: Keys.lastCheck) as? Date
        peerToPeerWiFi = defaults.bool(forKey: Keys.peerToPeer)
        extendScale = defaults.integer(forKey: Keys.extendScale)
        refreshLaunchAtLogin()
    }

    func refreshLaunchAtLogin() {
        launchAtLogin = SMAppService.mainApp.status == .enabled
    }

    func setLaunchAtLogin(_ on: Bool) {
        do {
            if on { try SMAppService.mainApp.register() } else { try SMAppService.mainApp.unregister() }
            launchAtLoginError = nil
        } catch {
            launchAtLoginError = error.localizedDescription
        }
        refreshLaunchAtLogin()
    }
}
