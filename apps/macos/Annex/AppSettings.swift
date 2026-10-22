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
    }

    init(defaults: UserDefaults = .standard) {
        self.defaults = defaults
        defaults.register(defaults: [Keys.hardware: true, Keys.fullScreen: true, Keys.control: true])
        preferHardwareEncoder = defaults.bool(forKey: Keys.hardware)
        startSharingAtLaunch = defaults.bool(forKey: Keys.autoShare)
        controlWhenViewing = defaults.bool(forKey: Keys.control)
        openFullScreen = defaults.bool(forKey: Keys.fullScreen)
        extendByDefault = defaults.bool(forKey: Keys.extend)
        customDeviceName = defaults.string(forKey: Keys.name)
        quality = defaults.integer(forKey: Keys.quality)
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
