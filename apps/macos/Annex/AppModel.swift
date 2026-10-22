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
    /// The main window's selected page.
    @Published var pane: Pane = .share

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
    /// Screen Recording permission (needed to share this screen).
    @Published private(set) var screenRecordingAllowed = CGPreflightScreenCaptureAccess()
    private var askedForScreenRecording = false
    /// Accessibility permission (needed to let viewers control this Mac).
    @Published private(set) var accessibilityTrusted = InputInjector.isTrusted
    /// Viewer: the host currently accepts this Mac's mouse and keyboard.
    @Published private(set) var controllingHost = false
    private var viewerPeerName = ""
    private var viewerFingerprint: String?
    /// Direct Wi-Fi: what's happening, for the Connect page.
    @Published private(set) var directStatus: String?
    /// Connected over the other computer's hotspot right now.
    @Published private(set) var onDirectWiFi = false
    /// Reconnecting over the hotspot after switching: who, how, and attempts left.
    private var directReconnect: (fingerprint: String, name: String, extend: Bool, attempts: Int)?
    private var cancellables: Set<AnyCancellable> = []

    private var core: CoreClient?
    private var discovery: DiscoveryHandle?
    private var host: HostController?
    private var viewer: ViewerHandle?
    private var viewerWindow: ViewerWindowController?

    /// Started by "Open at login": stay quietly in the menu bar.
    static let launchedAtLogin: Bool = {
        guard let e = NSAppleEventManager.shared().currentAppleEvent else { return false }
        return e.eventID == kAEOpenApplication
            && e.paramDescriptor(forKeyword: keyAEPropData)?.enumCodeValue == keyAELaunchedAsLogInItem
    }()

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

    func isPaired(_ peer: PeerInfo) -> Bool {
        peer.fingerprint.map { fp in trusted.contains { $0.fingerprint == fp } } ?? false
    }

    func refreshPermissions() {
        let recording = CGPreflightScreenCaptureAccess()
        if recording != screenRecordingAllowed { screenRecordingAllowed = recording }
        let trusted = InputInjector.isTrusted
        if trusted != accessibilityTrusted { accessibilityTrusted = trusted }
        settings.refreshLaunchAtLogin()
    }

    static var dataDirectory: URL {
        FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask)[0].appendingPathComponent("Annex", isDirectory: true)
    }

    private func start() {
        do {
            let name = settings.customDeviceName ?? Self.systemName
            let core = try CoreClient(dataDirectory: Self.dataDirectory, deviceName: name)
            self.core = core
            deviceName = core.deviceName
            // Show up under Nearby on other computers as soon as Annex is open.
            core.setPresence(true)
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

    static var systemName: String { Host.current().localizedName ?? "Mac" }

    /// Renames this Mac for other devices. An empty name goes back to the system name.
    func rename(_ name: String) {
        guard let core else { return }
        let trimmed = name.trimmingCharacters(in: .whitespacesAndNewlines)
        let stored = core.setDeviceName(trimmed.isEmpty ? Self.systemName : trimmed)
        settings.customDeviceName = trimmed.isEmpty ? nil : stored
        deviceName = stored
        core.setPresence(true)
        // Discovery advertises the name; re-advertise if we're sharing.
        if isHosting, hostStatus.viewerName == nil {
            stopHosting()
            startHosting()
        }
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
        // Ask macOS once per launch; after that the Share page explains what to do instead of
        // showing the system dialog every time sharing starts.
        screenRecordingAllowed = CGPreflightScreenCaptureAccess()
        if !screenRecordingAllowed && !askedForScreenRecording {
            askedForScreenRecording = true
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
                extend: extendMode, input: settings.controlWhenViewing, quality: settings.quality,
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
            viewerFingerprint = s.fingerprint
            if directReconnect != nil {
                directReconnect = nil
                directStatus = nil
                onDirectWiFi = true
            }
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
        case .directLinkOffer(_, let ssid, let passphrase):
            joinDirectLink(ssid: ssid, passphrase: passphrase, asViewer: true)
        case .directLinkUnavailable(_, let reason):
            directStatus = reason
            directReconnect = nil
        case .sessionEnded(_, let reason):
            if reason == "user" { onDirectWiFi = false }
            // Switching networks drops the old connection on purpose; reconnect instead of reporting it.
            if var d = directReconnect, d.attempts > 0 {
                d.attempts -= 1
                directReconnect = d
                DispatchQueue.main.asyncAfter(deadline: .now() + 2) { [weak self] in self?.reconnectDirect() }
                viewerStatus = nil
            } else {
                if directReconnect != nil {
                    directStatus = "Couldn't reach \(viewerPeerName) on the direct link. Connect again from Nearby."
                    directReconnect = nil
                }
                viewerStatus = reason == "user" ? nil : Self.friendlyReason(reason, peer: viewerPeerName)
            }
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

    // MARK: Direct Wi-Fi

    /// Viewer: ask the other computer (a Windows PC) to start its hotspot for a direct link.
    func switchToDirectWiFi() {
        guard let viewer else { return }
        directStatus = "Asking \(viewerPeerName) to start a direct Wi-Fi link…"
        viewer.requestDirectLink()
    }

    /// Joins the other computer's hotspot. As the viewer, reconnects to it afterwards; as the host,
    /// the viewer reconnects by itself once this Mac shows up on the hotspot.
    private func joinDirectLink(ssid: String, passphrase: String, asViewer: Bool) {
        directStatus = "Joining “\(ssid)”. The picture pauses for a few seconds while the Mac switches Wi-Fi."
        if asViewer, let fp = viewerFingerprint {
            directReconnect = (fp, viewerPeerName, extendMode, 4)
        }
        DispatchQueue.global(qos: .userInitiated).async { [weak self] in
            let error = DirectWiFi.join(ssid: ssid, passphrase: passphrase)
            DispatchQueue.main.async {
                guard let self else { return }
                if let error {
                    self.directReconnect = nil
                    self.directStatus = "Couldn't join automatically (\(error)). Join “\(ssid)” from the Wi-Fi menu (password \(passphrase)), then connect again."
                } else if !asViewer {
                    self.directStatus = "On “\(ssid)”, the direct link. The other computer reconnects by itself."
                } else if self.viewer == nil {
                    self.reconnectDirect()
                }
            }
        }
    }

    private func reconnectDirect() {
        guard let d = directReconnect else { return }
        extendMode = d.extend
        connect(address: "\(DirectWiFi.windowsHotspotAddress):47800", title: d.name, expectedFingerprint: d.fingerprint)
    }

    // MARK: Pairing

    private var pinPanels: [UInt64: PinPanel] = [:]

    private func handleSessionEvent(_ e: CoreEvent) {
        switch e {
        case .directLinkRequested(let session):
            host?.refuseDirectLink(session: session)
        case .inputRequested(_, let name, let fingerprint):
            NSApp.activate(ignoringOtherApps: true)
            let alert = NSAlert()
            alert.messageText = "Let \(name) control this Mac?"
            alert.informativeText = "\(name) wants to use this Mac's mouse and keyboard. You can change this any time in Paired Devices."
            alert.addButton(withTitle: "Allow")
            alert.addButton(withTitle: "Not Now")
            refreshTrusted()
            if alert.runModal() == .alertFirstButtonReturn,
               let peer = trusted.first(where: { $0.fingerprint == fingerprint }) {
                setAllowInput(peer, true)
            }
        case .directLinkOffer(_, let ssid, let passphrase):
            // A Windows viewer runs the hotspot; this Mac (the host) joins it.
            joinDirectLink(ssid: ssid, passphrase: passphrase, asViewer: false)
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

    /// Session-end codes from the core, in words.
    static func friendlyReason(_ reason: String, peer: String) -> String {
        switch reason {
        case "extend_not_supported":
            return "\(peer) can't add an extra display. On Windows it needs the Annex display driver (see Share this PC there). Try Show Its Screen instead."
        case "busy": return "\(peer) is already sharing with 4 computers."
        case "pairing_failed": return "The PIN didn't match. Try again with the PIN shown on the other computer."
        case "pairing_cancelled": return "Pairing was cancelled on the other computer."
        case "pairing_locked": return "Too many wrong PINs. Pairing is paused for 10 minutes."
        case "host_stopped": return "\(peer) stopped sharing."
        case "host_ended_session": return "\(peer) ended the session."
        case "closed", "connection closed": return "Disconnected from \(peer)."
        default:
            let r = reason.lowercased()
            if r.contains("refused") || r.contains("timed out") || r.contains("timeout") {
                return "Couldn't reach \(peer). Check that it's sharing and on the same network (\(reason))."
            }
            return reason
        }
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
            if pane == .peer(instance) { pane = .connect }
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
