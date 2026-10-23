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
    /// In-app updates from GitHub releases.
    lazy var updater = Updater(settings: settings)
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
    /// Viewer: connect, then switch to direct Wi-Fi before showing the picture.
    @Published var useDirectWiFi = false
    private var switchOnStart = false
    /// The viewer window waits for the direct link before it shows.
    private var holdingWindow = false
    /// Host: the hotspot this Mac joined for its viewers, if any.
    @Published private(set) var hostDirectWiFi: String?
    /// Nearby refresh in progress (the button shows a spinner).
    @Published private(set) var refreshingNearby = false
    /// Bumped whenever a viewer is replaced, so late events from the old one are ignored.
    private var viewerGeneration = 0
    private var cancellables: Set<AnyCancellable> = []
    private let localNetwork = LocalNetworkAccess()

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
        updater.objectWillChange.sink { [weak self] in self?.objectWillChange.send() }.store(in: &cancellables)
        updater.startAutomaticChecks()
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
            Annex.setLogEnabled(settings.saveLog)
            self.core = core
            deviceName = core.deviceName
            // Show up under Nearby on other computers as soon as Annex is open.
            core.setPresence(true)
            fingerprintDisplay = Annex.displayFingerprint(core.fingerprint)
            discovery = try DiscoveryHandle(core: core) { [weak self] e in
                DispatchQueue.main.async { self?.handleDiscovery(e) }
            }
            // Ask for Local Network access now, not when the first viewer connects.
            if !Self.isRunningTests { localNetwork.request() }
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
        hostDirectWiFi = nil
    }

    /// Host: ask a viewer (a Windows PC) to start its hotspot; this Mac joins it when it answers.
    func switchViewerToDirectWiFi(_ viewer: HostController.Viewer) {
        directStatus = "Asking \(viewer.name) to start direct Wi-Fi…"
        if host?.requestDirectLink(session: viewer.id) != true {
            directStatus = "\(viewer.name) is no longer connected."
        }
    }

    // MARK: Nearby

    /// Re-checks the Nearby list (computers that went away drop off, new ones appear).
    func refreshNearby() {
        guard !refreshingNearby else { return }
        refreshingNearby = true
        discovery?.refresh()
        DispatchQueue.main.asyncAfter(deadline: .now() + 3) { [weak self] in self?.refreshingNearby = false }
    }

    // MARK: Viewing

    func connect(to peer: PeerInfo) {
        guard let address = peer.connectAddress else { return }
        // Pin the certificate only for hosts we already trust; unknown hosts get a prompt.
        let pinned = peer.fingerprint.flatMap { fp in trusted.contains { $0.fingerprint == fp } ? fp : nil }
        connect(address: address, title: peer.name, expectedFingerprint: pinned)
        switchOnStart = useDirectWiFi && viewer != nil
    }

    func connectManual() {
        let address = manualAddress.trimmingCharacters(in: .whitespaces)
        guard !address.isEmpty else { return }
        connect(address: address, title: address, expectedFingerprint: nil)
    }

    private func connect(address: String, title: String, expectedFingerprint: String?) {
        guard let core else { return }
        disconnectViewer()
        switchOnStart = false
        holdingWindow = false
        let generation = viewerGeneration
        let window = ViewerWindowController(title: "Annex — \(title)")
        window.onClose = { [weak self] in self?.disconnectViewer() }
        do {
            let v = try ViewerHandle(
                core: core, address: address, expectedFingerprint: expectedFingerprint, display: Displays.mainDisplayInfo(),
                extend: extendMode, input: settings.controlWhenViewing, quality: settings.quality,
                onEvent: { [weak self] e in
                    DispatchQueue.main.async {
                        // A replaced viewer's last events (its sessionEnded) arrive after the new one started.
                        guard let self, self.viewerGeneration == generation else { return }
                        self.handleViewerEvent(e)
                    }
                },
                onFrame: { [weak window] buf, id, _, key in
                    window?.enqueue(Data(buf), frameID: id, keyframe: key)
                })
            window.attach(v)
            viewer = v
            viewerWindow = window
            viewerPeerName = title
            viewerStatus = "Connecting to \(title)…"
        } catch {
            viewerStatus = "Couldn't connect: \(error.localizedDescription)"
        }
    }

    func disconnectViewer() {
        // This viewer's own sessionEnded is ignored from here on (see viewerGeneration), so reset
        // what it would have reset.
        viewerGeneration += 1
        let w = viewerWindow
        viewerWindow = nil
        viewer?.disconnect()
        viewer = nil
        w?.onClose = nil
        w?.close()
        latency = nil
        controllingHost = false
        viewerStatus = nil
        onDirectWiFi = false
        holdingWindow = false
    }

    private func handleViewerEvent(_ e: CoreEvent) {
        switch e {
        case .sessionStarted(let s):
            let via = s.link == "direct" ? " · cable" : ""
            viewerStatus = (s.mode == "extend" ? "Extending \(s.peerName)" : "Viewing \(s.peerName)") + via
            viewerPeerName = s.peerName
            viewerFingerprint = s.fingerprint
            if directReconnect != nil {
                directReconnect = nil
                directStatus = nil
                onDirectWiFi = true
                holdingWindow = false
            }
            viewerWindow?.setStreamSize(width: s.stream.width, height: s.stream.height)
            if switchOnStart {
                // Set up direct Wi-Fi first; the picture starts once reconnected over it.
                switchOnStart = false
                holdingWindow = true
                switchToDirectWiFi()
            }
            if !holdingWindow { viewerWindow?.show(fullScreen: settings.openFullScreen) }
            controllingHost = s.input ?? false
            if settings.controlWhenViewing {
                viewerWindow?.setControlling(controllingHost, hostName: s.peerName)
            }
        case .streamConfig(let s):
            viewerWindow?.setStreamSize(width: s.width, height: s.height)
            viewerStatus = "Viewing \(viewerPeerName) · \(s.width)×\(s.height)"
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
            showHeldWindow()
        case .directLinkRequested:
            // A host asks this Mac to run the hotspot; only Windows can.
            viewer?.refuseDirectLink(reason: "This Mac can't run a hotspot. Start direct Wi-Fi from the Windows PC.")
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
                    directStatus = "Couldn't reach \(viewerPeerName) on direct Wi-Fi. Connect again from Nearby."
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
            holdingWindow = false
        default:
            handleSessionEvent(e)
        }
    }

    /// The direct link didn't happen: show the picture over the current network after all.
    private func showHeldWindow() {
        guard holdingWindow else { return }
        holdingWindow = false
        viewerWindow?.show(fullScreen: settings.openFullScreen)
    }

    // MARK: Direct Wi-Fi

    /// Viewer: ask the other computer (a Windows PC) to start its hotspot for a direct link.
    func switchToDirectWiFi() {
        guard let viewer else { return }
        directStatus = "Asking \(viewerPeerName) to start direct Wi-Fi…"
        viewer.requestDirectLink()
    }

    /// Joins the other computer's hotspot. As the viewer, reconnects to it afterwards; as the host,
    /// the viewer reconnects by itself once this Mac shows up on the hotspot.
    private func joinDirectLink(ssid: String, passphrase: String, asViewer: Bool) {
        directStatus = "Joining “\(ssid)”. The picture pauses for a moment."
        if asViewer, let fp = viewerFingerprint {
            directReconnect = (fp, viewerPeerName, extendMode, 4)
        }
        DispatchQueue.global(qos: .userInitiated).async { [weak self] in
            let error = DirectWiFi.join(ssid: ssid, passphrase: passphrase)
            DispatchQueue.main.async {
                guard let self else { return }
                if error != nil {
                    self.directReconnect = nil
                    self.directStatus = "Couldn't join automatically. Join “\(ssid)” from the Wi-Fi menu (password: \(passphrase)), then connect again."
                    self.showHeldWindow()
                } else if !asViewer {
                    self.hostDirectWiFi = ssid
                    self.directStatus = "On direct Wi-Fi (“\(ssid)”)."
                } else if self.directReconnect != nil {
                    // This Mac left the old network, so the old session is dead: drop it now
                    // instead of waiting for it to time out, and reconnect over the hotspot.
                    self.disconnectViewer()
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
        case .directLinkUnavailable(_, let reason):
            // Host: the viewer couldn't start a hotspot.
            directStatus = reason
        case .inputRequested(_, let name, let fingerprint):
            NSApp.activate(ignoringOtherApps: true)
            let alert = NSAlert()
            alert.messageText = "Let \(name) control this Mac?"
            alert.informativeText = "\(name) wants to use this Mac's mouse and keyboard. You can change this in Settings → Paired Devices."
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
            let panel = PinPanel(pin: p.pin, peerName: p.peerName,
                                 fingerprint: settings.showTechnicalDetails ? p.fingerprintDisplay : nil) { [weak self] in
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
        alert.informativeText = "Enter the PIN shown on \(req.peerName)."
            + (settings.showTechnicalDetails ? "\n\nIdentity code: \(req.fingerprintDisplay)" : "")
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
            return "\(peer) can't extend its display. Set up Extend on that computer, or use Show Its Screen."
        case "busy": return "\(peer) is already sharing with 4 computers."
        case "pairing_failed": return "Wrong PIN. Try again."
        case "pairing_cancelled": return "Pairing was cancelled on the other computer."
        case "pairing_locked": return "Too many wrong PINs. Try again in 10 minutes."
        case "host_stopped": return "\(peer) stopped sharing."
        case "host_ended_session": return "\(peer) ended the session."
        case "closed", "connection closed": return "Disconnected from \(peer)."
        default:
            let r = reason.lowercased()
            if r.contains("refused") || r.contains("timed out") || r.contains("timeout") {
                return "Couldn't reach \(peer). Check that it's sharing and on the same network."
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
