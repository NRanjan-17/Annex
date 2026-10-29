import AppKit
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
            MenuBarIcon(state: model.hostStatus.viewerName != nil ? .live : model.isViewing ? .viewing : model.isHosting ? .sharing : .off)
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
    func applicationDidFinishLaunching(_ notification: Notification) {
        SystemShortcuts.restoreLeftovers()
    }

    func applicationWillTerminate(_ notification: Notification) {
        SystemShortcuts.capture(false)
    }

    func applicationShouldHandleReopen(_ sender: NSApplication, hasVisibleWindows flag: Bool) -> Bool {
        NotificationCenter.default.post(name: .annexShowMainWindow, object: nil)
        return true
    }
}

private struct MenuBarIcon: View {
    let state: MenuBarGlyph.State
    @Environment(\.openWindow) private var openWindow

    var body: some View {
        Image(nsImage: MenuBarGlyph.image(state))
            .accessibilityLabel(MenuBarGlyph.label(state))
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
                // `ANNEX_PREVIEW_GLYPHS=dir`: the menu bar icon's states as PNGs, light and dark.
                if let dir = ProcessInfo.processInfo.environment["ANNEX_PREVIEW_GLYPHS"] {
                    MenuBarGlyph.writePreview(to: URL(fileURLWithPath: dir))
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
        // Real windows only: not the menu bar panel, PIN panels or alerts. A full-screen viewer has
        // no title bar but counts.
        let open = NSApp.windows.contains { w in
            w.isVisible && (w.styleMask.contains(.titled) || w is ViewerWindow) && !(w is NSPanel) && w.level == .normal
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

/// The menu bar icon, one look per state, so a glance says whether a session is on: faded while
/// nothing is connected (sharing or not), solid from the moment a connection succeeds.
/// The dot means only that: this Mac is being shown. Showing another computer gets a ▶ instead.
/// Template images: macOS tints them for light and dark menu bars.
enum MenuBarGlyph {
    enum State { case off, sharing, live, viewing }

    static func label(_ state: State) -> String {
        switch state {
        case .off: "Annex, not sharing"
        case .sharing: "Annex, sharing"
        case .live: "Annex, this screen is being shown"
        case .viewing: "Annex, showing another computer"
        }
    }

    private static var cache: [State: NSImage] = [:]

    static func image(_ state: State) -> NSImage {
        if let cached = cache[state] { return cached }
        let base = NSImage(named: "MenuBarIcon") ?? NSImage()
        let size = base.size == .zero ? NSSize(width: 18, height: 18) : base.size
        let image = NSImage(size: size, flipped: false) { rect in
            // Off and sharing with nobody connected: faded. Live: solid with a dot. Viewing: solid with a ▶.
            let connected = state == .live || state == .viewing
            base.draw(in: rect, from: .zero, operation: .sourceOver, fraction: connected ? 1 : 0.45)
            if state == .live || state == .viewing {
                let d = rect.width * (state == .live ? 0.34 : 0.4)
                let badge = NSRect(x: rect.maxX - d, y: rect.maxY - d, width: d, height: d)
                let path: NSBezierPath
                if state == .live {
                    path = NSBezierPath(ovalIn: badge)
                } else {
                    path = NSBezierPath()
                    path.move(to: NSPoint(x: badge.minX + d * 0.1, y: badge.minY))
                    path.line(to: NSPoint(x: badge.minX + d * 0.1, y: badge.maxY))
                    path.line(to: NSPoint(x: badge.maxX, y: badge.midY))
                    path.close()
                }
                // Clear a ring round the badge so it reads apart from the screens.
                NSGraphicsContext.current?.compositingOperation = .clear
                path.lineWidth = 3
                path.stroke()
                path.fill()
                NSGraphicsContext.current?.compositingOperation = .sourceOver
                NSColor.black.setFill()
                path.fill()
            }
            return true
        }
        image.isTemplate = true
        cache[state] = image
        return image
    }
}

#if DEBUG
extension MenuBarGlyph {
    /// Each state tinted the way the menu bar would, on a light and a dark strip, 4× size.
    static func writePreview(to dir: URL) {
        let states: [State] = [.off, .sharing, .live, .viewing]
        let scale: CGFloat = 4, cell: CGFloat = 30
        for (name, bg, fg) in [("light", NSColor(white: 0.93, alpha: 1), NSColor.black),
                               ("dark", NSColor(white: 0.12, alpha: 1), NSColor.white)] {
            let size = NSSize(width: cell * CGFloat(states.count) * scale, height: cell * scale)
            let rep = NSBitmapImageRep(bitmapDataPlanes: nil, pixelsWide: Int(size.width), pixelsHigh: Int(size.height),
                                       bitsPerSample: 8, samplesPerPixel: 4, hasAlpha: true, isPlanar: false,
                                       colorSpaceName: .deviceRGB, bytesPerRow: 0, bitsPerPixel: 0)!
            NSGraphicsContext.saveGraphicsState()
            NSGraphicsContext.current = NSGraphicsContext(bitmapImageRep: rep)
            bg.setFill()
            NSRect(origin: .zero, size: size).fill()
            for (i, state) in states.enumerated() {
                let glyph = image(state)
                let r = NSRect(x: (CGFloat(i) * cell + (cell - glyph.size.width) / 2) * scale,
                               y: (cell - glyph.size.height) / 2 * scale,
                               width: glyph.size.width * scale, height: glyph.size.height * scale)
                // Tint like a template: the colour, masked by the glyph's alpha.
                let tinted = NSImage(size: glyph.size, flipped: false) { rect in
                    glyph.draw(in: rect)
                    fg.set()
                    rect.fill(using: .sourceIn)
                    return true
                }
                tinted.draw(in: r)
            }
            NSGraphicsContext.restoreGraphicsState()
            try? rep.representation(using: .png, properties: [:])?.write(to: dir.appendingPathComponent("glyphs-\(name).png"))
        }
    }
}
#endif
