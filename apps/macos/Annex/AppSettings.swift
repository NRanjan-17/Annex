import Combine
import Foundation
import Security
import ServiceManagement

/// User preferences, persisted in UserDefaults (removed by "Uninstall Annex").
@MainActor
final class AppSettings: ObservableObject {
    private let defaults: UserDefaults

    @Published var preferHardwareEncoder: Bool { didSet { defaults.set(preferHardwareEncoder, forKey: Keys.hardware) } }
    @Published var startSharingAtLaunch: Bool { didSet { defaults.set(startSharingAtLaunch, forKey: Keys.autoShare) } }
    /// Nobody at this Mac's screen: pairing PINs also go to `pairing-pin.txt` in the data folder, and
    /// paired devices may control it without asking. Over SSH: `defaults write com.nranjan-17.annex headless -bool true`.
    @Published var headless: Bool { didSet { defaults.set(headless, forKey: Keys.headless) } }
    /// Viewer: ask hosts to let this Mac's mouse and keyboard control them. On by default: the
    /// sharing computer still decides per device, so asking is harmless.
    @Published var controlWhenViewing: Bool { didSet { defaults.set(controlWhenViewing, forKey: Keys.control) } }
    @Published var openFullScreen: Bool { didSet { defaults.set(openFullScreen, forKey: Keys.fullScreen) } }
    /// Viewer: scroll the other way on Windows and Linux, so this Mac's scrolling feels the same
    /// on them. On by default.
    @Published var reverseScrollOnOthers: Bool { didSet { defaults.set(reverseScrollOnOthers, forKey: Keys.reverseScroll) } }
    /// Copy on one computer, paste on the other, while one controls the other. On by default.
    @Published var shareClipboard: Bool { didSet { defaults.set(shareClipboard, forKey: Keys.clipboard) } }
    /// Take files the other computer sends, into Downloads. On by default.
    @Published var receiveFiles: Bool { didSet { defaults.set(receiveFiles, forKey: Keys.receiveFiles) } }
    /// Viewer: default to using the other computer as an extra display.
    @Published var extendByDefault: Bool { didSet { defaults.set(extendByDefault, forKey: Keys.extend) } }
    /// Viewer: bitrate ceiling asked of hosts. 0 best, 1 balanced, 2 fast (Wi-Fi).
    @Published var quality: Int { didSet { defaults.set(quality, forKey: Keys.quality) } }
    /// Show identity codes, addresses, stream sizes and latency in the UI. Off by default.
    @Published var showTechnicalDetails: Bool { didSet { defaults.set(showTechnicalDetails, forKey: Keys.technical) } }
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
    /// The rendezvous server (`wss://…`) that swaps the two computers' addresses; empty = Annex's.
    @Published var rendezvousServer: String { didSet { defaults.set(rendezvousServer, forKey: Keys.rendezvous) } }
    static let annexServer = "wss://connect.getannex.dev"
    /// The server codes use: the one set here, else Annex's.
    var server: String {
        let s = rendezvousServer.trimmingCharacters(in: .whitespaces)
        return s.isEmpty ? Self.annexServer : s
    }
    /// The user's own relay (`turn:host:port`) for networks that block direct connections; empty =
    /// the rendezvous server's relay, if it offers one.
    @Published var relayURL: String { didSet { defaults.set(relayURL, forKey: Keys.relayURL) } }
    @Published var relayUsername: String { didSet { defaults.set(relayUsername, forKey: Keys.relayUser) } }
    /// Kept in the Keychain, not in preferences.
    @Published var relayPassword: String { didSet { Keychain.set(relayPassword, account: Keys.relayPassword) } }
    @Published private(set) var launchAtLogin = false
    @Published private(set) var launchAtLoginError: String?

    private enum Keys {
        static let hardware = "preferHardwareEncoder"
        static let autoShare = "startSharingAtLaunch"
        static let headless = "headless"
        static let control = "useMouseAndKeyboard"
        static let fullScreen = "openFullScreen"
        static let clipboard = "shareClipboard"
        static let reverseScroll = "reverseScrollOnOthers"
        static let receiveFiles = "receiveFiles"
        static let extend = "extendByDefault"
        static let name = "deviceName"
        static let quality = "quality"
        static let technical = "showTechnicalDetails"
        static let updates = "checkForUpdates"
        static let beta = "betaUpdates"
        static let dismissedUpdate = "dismissedUpdateVersion"
        static let lastCheck = "lastUpdateCheck"
        static let peerToPeer = "peerToPeerWiFi"
        static let extendScale = "extendScale"
        static let rendezvous = "rendezvousServer"
        static let relayURL = "relayURL"
        static let relayUser = "relayUsername"
        static let relayPassword = "relayPassword"
    }

    /// The user's own relay, if they filled it in.
    var ownRelay: (url: String, username: String, password: String)? {
        let url = relayURL.trimmingCharacters(in: .whitespaces)
        return url.isEmpty ? nil : (url, relayUsername, relayPassword)
    }

    /// This copy is a pre-release (`0.2.0-pre.1`).
    static var runningBeta: Bool { Annex.version.contains("-") }

    init(defaults: UserDefaults = .standard) {
        self.defaults = defaults
        defaults.register(defaults: [Keys.hardware: true, Keys.fullScreen: true, Keys.control: true, Keys.updates: true,
                                       Keys.peerToPeer: true, Keys.clipboard: true, Keys.reverseScroll: true, Keys.receiveFiles: true])
        preferHardwareEncoder = defaults.bool(forKey: Keys.hardware)
        startSharingAtLaunch = defaults.bool(forKey: Keys.autoShare)
        headless = defaults.bool(forKey: Keys.headless)
        controlWhenViewing = defaults.bool(forKey: Keys.control)
        openFullScreen = defaults.bool(forKey: Keys.fullScreen)
        shareClipboard = defaults.bool(forKey: Keys.clipboard)
        reverseScrollOnOthers = defaults.bool(forKey: Keys.reverseScroll)
        receiveFiles = defaults.bool(forKey: Keys.receiveFiles)
        extendByDefault = defaults.bool(forKey: Keys.extend)
        customDeviceName = defaults.string(forKey: Keys.name)
        quality = defaults.integer(forKey: Keys.quality)
        showTechnicalDetails = defaults.bool(forKey: Keys.technical)
        checkForUpdates = defaults.bool(forKey: Keys.updates)
        // Saved only when the user flips it. Until then it follows what's installed: someone who
        // installed a beta by hand gets betas, not an offer to go back to the last normal release.
        betaUpdates = defaults.object(forKey: Keys.beta) as? Bool ?? Self.runningBeta
        dismissedUpdateVersion = defaults.string(forKey: Keys.dismissedUpdate)
        lastUpdateCheck = defaults.object(forKey: Keys.lastCheck) as? Date
        peerToPeerWiFi = defaults.bool(forKey: Keys.peerToPeer)
        rendezvousServer = defaults.string(forKey: Keys.rendezvous) ?? ""
        relayURL = defaults.string(forKey: Keys.relayURL) ?? ""
        relayUsername = defaults.string(forKey: Keys.relayUser) ?? ""
        relayPassword = Keychain.get(account: Keys.relayPassword) ?? ""
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

/// Small secrets in the login Keychain, under the app's bundle ID (which "Uninstall Annex" removes).
enum Keychain {
    private static var service: String { Bundle.main.bundleIdentifier ?? "Annex" }

    static func get(account: String) -> String? {
        let query: [String: Any] = [kSecClass as String: kSecClassGenericPassword, kSecAttrService as String: service,
                                    kSecAttrAccount as String: account, kSecReturnData as String: true]
        var out: CFTypeRef?
        guard SecItemCopyMatching(query as CFDictionary, &out) == errSecSuccess, let data = out as? Data else { return nil }
        return String(data: data, encoding: .utf8)
    }

    static func set(_ value: String, account: String) {
        let query: [String: Any] = [kSecClass as String: kSecClassGenericPassword, kSecAttrService as String: service,
                                    kSecAttrAccount as String: account]
        SecItemDelete(query as CFDictionary)
        guard !value.isEmpty else { return }
        var add = query
        add[kSecValueData as String] = Data(value.utf8)
        SecItemAdd(add as CFDictionary, nil)
    }
}
