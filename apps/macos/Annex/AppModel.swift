import AppKit
import Combine
import Foundation

/// App state shared by the menu bar UI. Owns the core, discovery, hosting and viewer windows.
@MainActor
final class AppModel: ObservableObject {
    @Published private(set) var deviceName = ""
    @Published private(set) var fingerprintDisplay = ""
    @Published private(set) var startupError: String?

    let settings = AppSettings()

    @Published var displays: [Displays.Info] = []
    @Published var selectedDisplayID: CGDirectDisplayID?
    @Published private(set) var isHosting = false
    @Published private(set) var hostStatus = HostController.Status()

    @Published private(set) var discovered: [PeerInfo] = []
    @Published var manualAddress = ""
    /// Viewer: ask hosts for a new virtual display instead of mirroring their screen.
    @Published var extendMode = false
    @Published private(set) var viewerStatus: String?
    @Published private(set) var latency: LatencyStats?
    @Published private(set) var trusted: [TrustedPeer] = []
    /// Accessibility permission (needed to let viewers control this Mac).
    @Published private(set) var accessibilityTrusted = InputInjector.isTrusted
    /// Viewer: the host currently accepts this Mac's mouse and keyboard.
    @Published private(set) var controllingHost = false
    private var viewerPeerName = ""
    private var cancellables: Set<AnyCancellable> = []

    private var core: CoreClient?
    private var discovery: DiscoveryHandle?
    private var host: HostController?
    private var viewer: ViewerHandle?
    private var viewerWindow: ViewerWindowController?

    static var isRunningTests: Bool {
        ProcessInfo.processInfo.environment["XCTestConfigurationFilePath"] != nil
    }

    init() {
        extendMode = settings.extendByDefault
        // Re-publish settings changes so views observing the model refresh.
        settings.objectWillChange.sink { [weak self] in self?.objectWillChange.send() }.store(in: &cancellables)
        guard !Self.isRunningTests else { return }
        start()
        NotificationCenter.default.publisher(for: NSApplication.didBecomeActiveNotification)
            .sink { [weak self] _ in self?.refreshPermissions() }
            .store(in: &cancellables)
        if settings.startSharingAtLaunch { startHosting() }
    }

    var isViewing: Bool { viewer != nil }

    func refreshPermissions() {
        accessibilityTrusted = InputInjector.isTrusted
        settings.refreshLaunchAtLogin()
    }

    static var dataDirectory: URL {
        FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask)[0].appendingPathComponent("Annex", isDirectory: true)
    }

    private func start() {
        do {
            let name = Host.current().localizedName ?? "Mac"
            let core = try CoreClient(dataDirectory: Self.dataDirectory, deviceName: name)
            self.core = core
            deviceName = core.deviceName
            fingerprintDisplay = Annex.displayFingerprint(core.fingerprint)
            discovery = try DiscoveryHandle(core: core) { [weak self] e in
                DispatchQueue.main.async { self?.handleDiscovery(e) }
            }
        } catch {
            startupError = error.localizedDescription
        }
        refreshDisplays()
        refreshTrusted()
    }

    func refreshDisplays() {
        displays = Displays.all()
        if selectedDisplayID == nil || !displays.contains(where: { $0.id == selectedDisplayID }) {
            selectedDisplayID = displays.first?.id
        }
    }

    func refreshTrusted() {
        trusted = core?.trustedPeers ?? []
    }

    // MARK: Hosting

    func setHosting(_ on: Bool) {
        on ? startHosting() : stopHosting()
    }

    private func startHosting() {
        guard let core, host == nil,
              let display = displays.first(where: { $0.id == selectedDisplayID }) ?? displays.first else { return }
        if !CGPreflightScreenCaptureAccess() {
            CGRequestScreenCaptureAccess()
        }
        let h = HostController(core: core, display: display, preferHardware: settings.preferHardwareEncoder)
        h.onStatus = { [weak self] s in self?.hostStatus = s }
        h.onEvent = { [weak self] e in self?.handleSessionEvent(e) }
        do {
            try h.start()
            host = h
            isHosting = true
            hostStatus = h.status
        } catch {
            hostStatus = HostController.Status(error: error.localizedDescription)
        }
    }

    private func stopHosting() {
        host?.stop()
        host = nil
        isHosting = false
        hostStatus = HostController.Status()
    }

    // MARK: Viewing

    func connect(to peer: PeerInfo) {
        guard let address = peer.connectAddress else { return }
        // Pin the certificate only for hosts we already trust; unknown hosts get a prompt.
        let pinned = peer.fingerprint.flatMap { fp in trusted.contains { $0.fingerprint == fp } ? fp : nil }
        connect(address: address, title: peer.name, expectedFingerprint: pinned)
    }

    func connectManual() {
        let address = manualAddress.trimmingCharacters(in: .whitespaces)
        guard !address.isEmpty else { return }
        connect(address: address, title: address, expectedFingerprint: nil)
    }

    private func connect(address: String, title: String, expectedFingerprint: String?) {
        guard let core else { return }
        disconnectViewer()
        let window = ViewerWindowController(title: "Annex — \(title)")
        window.onClose = { [weak self] in self?.disconnectViewer() }
        do {
            let v = try ViewerHandle(
                core: core, address: address, expectedFingerprint: expectedFingerprint, display: Displays.mainDisplayInfo(),
                extend: extendMode, input: settings.controlWhenViewing,
                onEvent: { [weak self] e in DispatchQueue.main.async { self?.handleViewerEvent(e) } },
                onFrame: { [weak window] buf, id, _, key in
                    window?.enqueue(Data(buf), frameID: id, keyframe: key)
                })
            window.attach(v)
            viewer = v
            viewerWindow = window
            viewerPeerName = title
            viewerStatus = "Connecting to \(title)…"
        } catch {
            viewerStatus = "Connect failed: \(error.localizedDescription)"
        }
    }

    func disconnectViewer() {
        let w = viewerWindow
        viewerWindow = nil
        viewer?.disconnect()
        viewer = nil
        w?.onClose = nil
        w?.close()
        latency = nil
        controllingHost = false
    }

    private func handleViewerEvent(_ e: CoreEvent) {
        switch e {
        case .sessionStarted(let s):
            let via = s.connectionDescription.isEmpty ? "" : " (\(s.connectionDescription))"
            viewerStatus = (s.mode == "extend" ? "Extending \(s.peerName)" : "Viewing \(s.peerName)") + via
            viewerPeerName = s.peerName
            viewerWindow?.setStreamSize(width: s.stream.width, height: s.stream.height)
            viewerWindow?.show(fullScreen: settings.openFullScreen)
            controllingHost = s.input ?? false
            if settings.controlWhenViewing {
                viewerWindow?.setControlling(controllingHost, hostName: s.peerName)
            }
        case .streamConfig(let s):
            viewerWindow?.setStreamSize(width: s.width, height: s.height)
            viewerStatus = "Viewing \(viewerPeerName) — \(s.width)×\(s.height) @ \(s.fps) fps"
        case .inputAllowed(_, let allowed):
            controllingHost = allowed
            viewerWindow?.setControlling(allowed, hostName: viewerPeerName)
        case .latencyStats(let l):
            latency = l
        case .sessionEnded(_, let reason):
            viewerStatus = reason == "user" ? nil : "Disconnected: \(reason)"
            viewer = nil
            let w = viewerWindow
            viewerWindow = nil
            w?.onClose = nil
            w?.close()
            latency = nil
            controllingHost = false
        default:
            handleSessionEvent(e)
        }
    }

    // MARK: Pairing

    private var pinPanels: [UInt64: PinPanel] = [:]

    private func handleSessionEvent(_ e: CoreEvent) {
        switch e {
        case .pairingPin(let p):
            // Host: show the PIN until pairing finishes or the user cancels.
            let panel = PinPanel(pin: p.pin, peerName: p.peerName, fingerprint: p.fingerprintDisplay) { [weak self] in
                self?.core?.cancelRequest(p.requestId)
            }
            pinPanels[p.requestId] = panel
            panel.show()
        case .pairingPinRequired(let r):
            promptForPin(r)
        case .pairingFinished(let f):
            pinPanels.removeValue(forKey: f.requestId)?.close()
            if !f.success {
                notify("Pairing with \(f.peerName) failed", "The PIN didn't match or pairing was cancelled.")
            }
            refreshTrusted()
        case .sessionStarted, .sessionEnded:
            refreshTrusted()
        default:
            break
        }
    }

    private func promptForPin(_ req: PairingPinRequired) {
        guard let core else { return }
        NSApp.activate(ignoringOtherApps: true)
        let alert = NSAlert()
        alert.messageText = "Pair with “\(req.peerName)”"
        alert.informativeText = "Enter the 6-digit PIN shown on \(req.peerName).\n\nIts identity code: \(req.fingerprintDisplay)"
        let field = NSTextField(frame: NSRect(x: 0, y: 0, width: 160, height: 28))
        field.placeholderString = "123 456"
        field.font = .monospacedDigitSystemFont(ofSize: 18, weight: .medium)
        field.alignment = .center
        alert.accessoryView = field
        alert.addButton(withTitle: "Pair")
        alert.addButton(withTitle: "Cancel")
        alert.window.initialFirstResponder = field
        if alert.runModal() == .alertFirstButtonReturn {
            core.submitPin(requestID: req.requestId, pin: field.stringValue)
        } else {
            core.cancelRequest(req.requestId)
        }
    }

    private func notify(_ title: String, _ text: String) {
        let alert = NSAlert()
        alert.messageText = title
        alert.informativeText = text
        alert.alertStyle = .informational
        NSApp.activate(ignoringOtherApps: true)
        alert.runModal()
    }

    /// Host: let a trusted device control this Mac's mouse and keyboard (or stop it).
    func setAllowInput(_ peer: TrustedPeer, _ allow: Bool) {
        if allow && !InputInjector.isTrusted {
            InputInjector.requestTrust()
        }
        _ = try? core?.setAllowInput(peer.fingerprint, allow)
        refreshTrusted()
        refreshPermissions()
    }

    func revoke(_ peer: TrustedPeer) {
        _ = try? core?.untrust(peer.fingerprint)
        refreshTrusted()
    }

    // MARK: Discovery

    private func handleDiscovery(_ e: CoreEvent) {
        switch e {
        case .peerFound(let p):
            discovered.removeAll { $0.instance == p.instance }
            discovered.append(p)
            discovered.sort { $0.name.localizedCaseInsensitiveCompare($1.name) == .orderedAscending }
        case .peerLost(let instance):
            discovered.removeAll { $0.instance == instance }
        default:
            break
        }
    }

    func quit() {
        disconnectViewer()
        stopHosting()
        NSApp.terminate(nil)
    }
}
