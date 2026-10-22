import SwiftUI

@main
struct AnnexApp: App {
    @StateObject private var model = AppModel()

    var body: some Scene {
        MenuBarExtra {
            MenuContent(model: model)
                .onAppear { model.refreshDisplays() }
        } label: {
            MenuBarIcon(hosting: model.isHosting)
        }
        .menuBarExtraStyle(.window)

        // System Settings-style window: sidebar plus a unified toolbar.
        Window("Annex Settings", id: "settings") {
            SettingsView(model: model)
        }
        .windowResizability(.contentSize)
        .windowToolbarStyle(.unified)
        .defaultPosition(.center)
        .commands { CommandGroup(replacing: .appSettings) { SettingsCommand() } }

        #if DEBUG
        // The menu panel as a normal window, for screenshots (`ANNEX_PREVIEW_UI=1`).
        Window("Annex Menu", id: "menu-preview") {
            MenuContent(model: model).background(.background)
        }
        .windowResizability(.contentSize)
        #endif
    }
}

private struct MenuBarIcon: View {
    let hosting: Bool
    @Environment(\.openWindow) private var openWindow

    var body: some View {
        Image(systemName: hosting ? "display.2" : "display")
            .onAppear {
                DockPresence.shared.start()
                #if DEBUG
                if ProcessInfo.processInfo.environment["ANNEX_PREVIEW_UI"] != nil {
                    openWindow(id: "menu-preview")
                    openWindow(id: "settings")
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
        guard NSApp.activationPolicy() != policy else { return }
        NSApp.setActivationPolicy(policy)
        if open { NSApp.activate(ignoringOtherApps: true) }
    }
}

/// ⌘, opens the settings window.
private struct SettingsCommand: View {
    @Environment(\.openWindow) private var openWindow

    var body: some View {
        Button("Settings…") {
            openWindow(id: "settings")
            NSApp.activate(ignoringOtherApps: true)
        }
        .keyboardShortcut(",")
    }
}
