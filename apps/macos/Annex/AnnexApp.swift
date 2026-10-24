import SwiftUI

@main
struct AnnexApp: App {
    @NSApplicationDelegateAdaptor(AppDelegate.self) private var appDelegate
    @StateObject private var model = AppModel()

    var body: some Scene {
        MenuBarExtra {
            MenuContent(model: model)
                .onAppear { model.refreshDisplays() }
        } label: {
            MenuBarIcon(hosting: model.isHosting)
        }
        .menuBarExtraStyle(.window)

        // The app window: a glass sidebar and pages over a living backdrop.
        Window("Annex", id: "main") {
            MainView(model: model)
        }
        .defaultSize(width: 1080, height: 700)
        .windowStyle(.hiddenTitleBar)
        .defaultPosition(.center)
        .commands { CommandGroup(replacing: .appSettings) { SettingsCommand(model: model) } }

        #if DEBUG
        // The menu panel as a normal window, for screenshots (`ANNEX_PREVIEW_UI=1`).
        Window("Annex Menu", id: "menu-preview") {
            MenuContent(model: model).background(.background)
        }
        .windowResizability(.contentSize)
        #endif
    }
}

extension Notification.Name {
    /// Ask the app to show its main window (Dock click, opening it again from Finder).
    static let annexShowMainWindow = Notification.Name("annexShowMainWindow")
}

/// Clicking Annex in the Dock while it runs without a window: SwiftUI only reopens WindowGroups
/// by itself, so bring the Annex window back here.
final class AppDelegate: NSObject, NSApplicationDelegate {
    func applicationShouldHandleReopen(_ sender: NSApplication, hasVisibleWindows flag: Bool) -> Bool {
        NotificationCenter.default.post(name: .annexShowMainWindow, object: nil)
        return true
    }
}

private struct MenuBarIcon: View {
    let hosting: Bool
    @Environment(\.openWindow) private var openWindow

    var body: some View {
        // Template image: macOS tints it for light and dark menu bars.
        Image("MenuBarIcon")
            .opacity(hosting ? 1 : 0.85)
            .onReceive(NotificationCenter.default.publisher(for: .annexShowMainWindow)) { _ in
                openWindow(id: "main")
                NSApp.activate(ignoringOtherApps: true)
            }
            .onAppear {
                DockPresence.shared.start()
                // Open the window on launch, except when started at login (quietly, in the menu bar).
                if !AppModel.isRunningTests && !AppModel.launchedAtLogin {
                    openWindow(id: "main")
                }
                #if DEBUG
                if ProcessInfo.processInfo.environment["ANNEX_PREVIEW_UI"] != nil {
                    openWindow(id: "menu-preview")
                }
                #endif
            }
    }
}

/// Annex lives in the menu bar, but while one of its windows is open (Settings, a viewer) it
/// shows in the Dock and the app switcher like any other app, with its icon.
@MainActor
final class DockPresence {
    static let shared = DockPresence()
    private var observers: [NSObjectProtocol] = []

    func start() {
        guard observers.isEmpty else { return }
        let center = NotificationCenter.default
        for name in [NSWindow.didBecomeKeyNotification, NSWindow.willCloseNotification, NSWindow.didMiniaturizeNotification] {
            observers.append(center.addObserver(forName: name, object: nil, queue: .main) { _ in
                // willClose fires before the window is gone; look again on the next turn.
                DispatchQueue.main.async { DockPresence.shared.update() }
            })
        }
    }

    private func update() {
        // Real windows only: not the menu bar panel, PIN panels or alerts.
        let open = NSApp.windows.contains { w in
            w.isVisible && w.styleMask.contains(.titled) && !(w is NSPanel) && w.level == .normal
        }
        let policy: NSApplication.ActivationPolicy = open ? .regular : .accessory
        // Switching policy (or a window opening while Annex is menu-bar only) takes full screen
        // away from the main window; give it back every time.
        for w in NSApp.windows where w.identifier == WindowChrome.id { WindowChrome.allowFullScreen(w) }
        guard NSApp.activationPolicy() != policy else { return }
        NSApp.setActivationPolicy(policy)
        for w in NSApp.windows where w.identifier == WindowChrome.id { WindowChrome.allowFullScreen(w) }
        if open { NSApp.activate(ignoringOtherApps: true) }
    }
}

/// ⌘, opens the settings window.
private struct SettingsCommand: View {
    let model: AppModel
    @Environment(\.openWindow) private var openWindow

    var body: some View {
        Button("Settings…") {
            model.pane = .settings
            openWindow(id: "main")
            NSApp.activate(ignoringOtherApps: true)
        }
        .keyboardShortcut(",")
    }
}
