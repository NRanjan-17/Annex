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
    @Published var pane: Pane = .home

    @Published var displays: [Displays.Info] = []
    @Published var selectedDisplayID: CGDirectDisplayID?
    @Published private(set) var isHosting = false
    @Published private(set) var hostStatus = HostController.Status() {
        didSet {
            updateKeepAwake()
            updatePeerToPeerVisibility()
            updateInSession()
        }
    }
    /// Host: every Mac nearby can find this one on peer-to-peer Wi-Fi until then (else only
    /// paired Macs can).
    @Published private(set) var visibleToNewMacsUntil: Date?

    /// Nearby: computers found on the network, plus Macs found only on peer-to-peer Wi-Fi.
    @Published private(set) var discovered: [PeerInfo] = []
    private var lanPeers: [PeerInfo] = []
    /// Macs sharing on peer-to-peer Wi-Fi (no router needed), while the Connect page looks for them.
    private var peerToPeerHosts: [PeerToPeerHost] = []
    private var peerToPeerBrowser: PeerToPeerBrowser?
    private var peerToPeerAdvertiser: PeerToPeerAdvertiser?
    /// The current viewer's relay over peer-to-peer Wi-Fi, if it has one.
    private var peerToPeerTunnel: PeerToPeerTunnel?
    @Published var manualAddress = ""
    /// Connect by Code or Address, open over Nearby (the menu bar panel asks for it too).
    @Published var showConnectSheet = false
    /// Connect by Address sheet: a code from a computer sharing over the internet.
    @Published var manualCode = ""
    /// The current viewer connected by code (its errors read differently).
    private var viewingByCode = false
    /// Sharing over the internet: what the This Mac page shows.
    @Published private(set) var internetShare: InternetShare?
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
    /// Bumped whenever a viewer is replaced, so late events from the old one are ignored.
    private var viewerGeneration = 0
    private var cancellables: Set<AnyCancellable> = []
    private let localNetwork = LocalNetworkAccess()

    private var core: CoreClient?
    private var discovery: DiscoveryHandle?
    private var host: HostController?
    private var viewer: ViewerHandle? { didSet { updateKeepAwake(); updateInSession() } }
    /// Viewing, or showing this Mac to someone. Peer-to-peer browsing pauses meanwhile: it shares
    /// the Wi-Fi radio with the session and would add lag.
    @Published private(set) var inSession = false
    /// Held while a session runs, so neither screen sleeps: the viewer gets no local input, and
    /// the host's screen (or its Annex display) is what the viewer is watching.
    private var keepAwake: NSObjectProtocol?

    private func updateKeepAwake() {
        let active = viewer != nil || !hostStatus.viewers.isEmpty || !hostStatus.extendViewers.isEmpty
        if active, keepAwake == nil {
            keepAwake = ProcessInfo.processInfo.beginActivity(
                options: [.idleDisplaySleepDisabled, .idleSystemSleepDisabled], reason: "An Annex session is running")
        } else if !active, let token = keepAwake {
            ProcessInfo.processInfo.endActivity(token)
            keepAwake = nil
        }
    }
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
    /// A session either way: this Mac shows another computer, or another computer shows this one.
    var isLive: Bool { isViewing || hostStatus.viewerName != nil }

    private func updateInSession() {
        let busy = viewer != nil || hostStatus.viewerName != nil
        if busy != inSession { inSession = busy }
    }

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
        #if DEBUG
        // Its own identity and pairings, kept in a file (see `start`), so a Debug build never
        // touches the installed app's.
        let folder = "Annex Debug"
        #else
        let folder = "Annex"
        #endif
        return FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask)[0].appendingPathComponent(folder, isDirectory: true)
    }

    private func start() {
        do {
            #if DEBUG
            // Debug builds are signed ad hoc, so each rebuild is a new app to the Keychain and
            // macOS asks for the login password on every launch. Keep their key in a file, in
            // their own data folder.
            setenv("ANNEX_KEY_STORAGE", "file", 0)
            #endif
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
            $pane.combineLatest(settings.$peerToPeerWiFi, $inSession.removeDuplicates())
                .sink { [weak self] pane, on, busy in self?.updatePeerToPeerBrowsing(pane: pane, on: on && !busy) }
                .store(in: &cancellables)
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
            if settings.peerToPeerWiFi, let port = h.status.listeningPort.flatMap({ UInt16(exactly: $0) }), port > 0 {
                do {
                    peerToPeerAdvertiser = try PeerToPeerAdvertiser(name: core.deviceName, fingerprint: core.fingerprint,
                                                                    extend: HostController.extendSupported, corePort: port,
                                                                    visibility: peerToPeerVisibility)
                } catch {
                    Annex.log("p2p: couldn't advertise: \(error)", error: true)
                }
            }
        } catch {
            hostStatus = HostController.Status(error: error.localizedDescription)
        }
    }

    private func stopHosting() {
        internetShare = nil
        peerToPeerAdvertiser?.stop()
        peerToPeerAdvertiser = nil
        visibleToNewMacsUntil = nil
        host?.stop()
        host = nil
        isHosting = false
        hostStatus = HostController.Status()
        hostDirectWiFi = nil
    }

    // MARK: Internet

    /// Where a code-sharing attempt is.
    enum InternetShare: Equatable {
        case starting
        case code(String, expires: Date)
        case joining(String)
        /// The viewer couldn't connect directly. `relayed`: it got a relay address (`limited`: ours).
        case relay(String, relayed: Bool, limited: Bool)
        /// The code no longer works: why (nil when it was used).
        case ended(String?)
    }

    var internetReady: Bool {
        settings.internetEnabled && !settings.rendezvousServer.trimmingCharacters(in: .whitespaces).isEmpty
    }

    /// Host: get a code for a computer on another network to type.
    func shareCode() {
        guard let host, internetReady else { return }
        internetShare = .starting
        do {
            try host.shareCode(server: settings.rendezvousServer.trimmingCharacters(in: .whitespaces),
                               ownRelay: settings.ownRelay)
        } catch {
            internetShare = .ended(error.localizedDescription)
        }
    }

    func stopCode() {
        host?.stopCode()
        internetShare = nil
    }

    private func handleInternetEvent(_ e: CoreEvent) {
        switch e {
        case .internetCode(let code, let seconds):
            internetShare = .code(code, expires: Date().addingTimeInterval(TimeInterval(seconds)))
        case .internetJoined(let name):
            internetShare = .joining(name)
        case .internetRelay(let relayed, let limited):
            if case .joining(let name) = internetShare {
                internetShare = .relay(name, relayed: relayed, limited: limited)
            }
        case .internetCodeEnded(let reason):
            switch (reason, internetShare) {
            case ("stopped", _): internetShare = nil
            case ("used", .relay(_, false, _)):
                internetShare = .ended("The other computer's network blocks direct connections, and no relay was available. "
                                       + "Add your own relay in Settings → Internet.")
            case ("used", _): internetShare = .ended(nil)
            case ("expired", _): internetShare = .ended("The code expired.")
            case (let r, _): internetShare = .ended(Self.friendlyInternetError(r))
            }
        default: break
        }
    }

    static func friendlyInternetError(_ r: String) -> String {
        let l = r.lowercased()
        if l.contains("stun") || l.contains("didn't answer") {
            return "Couldn't find this Mac's internet address. Check the internet connection."
        }
        if l.contains("rendezvous") || l.contains("websocket") || l.contains("i/o") || l.contains("dns") {
            return "Couldn't reach the server. Check the address in Settings → Internet."
        }
        return r
    }

    /// Viewer: connect by a code from a computer sharing over the internet.
    func connectByCode() {
        let code = manualCode.trimmingCharacters(in: .whitespaces)
        guard let core, internetReady, !code.isEmpty else { return }
        disconnectViewer()
        switchOnStart = false
        holdingWindow = false
        let generation = viewerGeneration
        let window = ViewerWindowController(title: "Code \(code.uppercased())")
        window.onClose = { [weak self] in self?.disconnectViewer() }
        do {
            let v = try ViewerHandle(
                core: core, code: code, server: settings.rendezvousServer.trimmingCharacters(in: .whitespaces),
                display: Displays.mainDisplayInfo(), extend: extendMode, input: settings.controlWhenViewing,
                quality: settings.quality,
                onEvent: { [weak self] e in
                    DispatchQueue.main.async {
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
            viewerPeerName = "the other computer"
            viewerStatus = "Connecting with code \(code.uppercased())…"
            viewingByCode = true
        } catch {
            viewerStatus = "Couldn't connect: \(error.localizedDescription)"
        }
    }

    #if DEBUG
    /// Screenshots: `ANNEX_PREVIEW_PANE=share|settings` opens that page; with `ANNEX_PREVIEW_CODE=1`
    /// (and sharing on) it also gets a code. `ANNEX_PREVIEW_CONNECT=address` connects there.
    func openPreviewPane() {
        let env = ProcessInfo.processInfo.environment
        if let address = env["ANNEX_PREVIEW_CONNECT"] {
            DispatchQueue.main.asyncAfter(deadline: .now() + 2) { [weak self] in
                self?.manualAddress = address
                self?.connectManual()
            }
        }
        switch env["ANNEX_PREVIEW_PANE"] {
        case "share": pane = .share
        case "settings": pane = .settings
        default: return
        }
        if env["ANNEX_PREVIEW_CODE"] != nil {
            DispatchQueue.main.asyncAfter(deadline: .now() + 3) { [weak self] in self?.shareCode() }
        }
    }
    #endif

    // MARK: Peer-to-peer visibility

    /// How long "Let a new Mac find this one" lasts.
    static let newMacWindow: TimeInterval = 5 * 60

    /// Host: for a few minutes, every Mac nearby can see this one on peer-to-peer Wi-Fi. It still
    /// has to pair with the PIN shown here.
    func letNewMacsFindThisOne() {
        let until = Date().addingTimeInterval(Self.newMacWindow)
        visibleToNewMacsUntil = until
        updatePeerToPeerVisibility()
        DispatchQueue.main.asyncAfter(deadline: .now() + Self.newMacWindow) { [weak self] in
            guard let self, self.visibleToNewMacsUntil == until else { return }
            self.hideFromNewMacs()
        }
    }

    func hideFromNewMacs() {
        visibleToNewMacsUntil = nil
        updatePeerToPeerVisibility()
    }

    /// Peer-to-peer Wi-Fi slows the normal Wi-Fi link while it's on, which would hurt a session
    /// streaming over that link, so this Mac stops advertising while any session runs (flows
    /// already relayed keep going). The user asking for new Macs overrides that.
    private var peerToPeerVisibility: PeerToPeer.Visibility {
        if let until = visibleToNewMacsUntil, until > Date() { return .everyone }
        let busy = !hostStatus.viewers.isEmpty || !hostStatus.extendViewers.isEmpty
        return busy ? .hidden : .paired
    }

    private func updatePeerToPeerVisibility() {
        peerToPeerAdvertiser?.setVisibility(peerToPeerVisibility)
    }

    /// Host: ask a viewer (a Windows PC) to start its hotspot; this Mac joins it when it answers.
    func switchViewerToDirectWiFi(_ viewer: HostController.Viewer) {
        directStatus = "Asking \(viewer.name) to start direct Wi-Fi…"
        if host?.requestDirectLink(session: viewer.id) != true {
            directStatus = "\(viewer.name) is no longer connected."
        }
    }

    // MARK: Viewing

    func connect(to peer: PeerInfo) {
        // Pin the certificate only for hosts we already trust; unknown hosts get a prompt.
        let pinned = peer.fingerprint.flatMap { fp in trusted.contains { $0.fingerprint == fp } ? fp : nil }
        let useDirect = useDirectWiFi
        guard settings.peerToPeerWiFi, let fp = peer.fingerprint,
              let p2p = peerToPeerHosts.first(where: { $0.fingerprint == fp }) else {
            guard let address = peer.connectAddress else { return }
            connect(address: address, title: peer.name, expectedFingerprint: pinned)
            switchOnStart = useDirect && viewer != nil
            return
        }
        // Also reachable over peer-to-peer Wi-Fi: open the relay first, then let the core race
        // it against the network addresses.
        disconnectViewer()
        let generation = viewerGeneration
        viewerStatus = "Connecting to \(peer.name)…"
        var tunnel: PeerToPeerTunnel?
        tunnel = try? PeerToPeerTunnel(to: p2p.endpoint) { [weak self] local in
            guard let self, self.viewerGeneration == generation else { tunnel?.stop(); return }
            let address = [peer.connectAddress, local].compactMap { $0 }.joined(separator: ",")
            guard !address.isEmpty else {
                tunnel?.stop()
                self.viewerStatus = "Couldn't reach \(peer.name) over peer-to-peer Wi-Fi."
                return
            }
            self.connect(address: address, title: peer.name, expectedFingerprint: pinned, tunnel: local == nil ? nil : tunnel)
            if local == nil { tunnel?.stop() }
            self.switchOnStart = useDirect && self.viewer != nil
        }
        if tunnel == nil, let address = peer.connectAddress {
            connect(address: address, title: peer.name, expectedFingerprint: pinned)
            switchOnStart = useDirect && viewer != nil
        }
    }

    func connectManual() {
        let address = manualAddress.trimmingCharacters(in: .whitespaces)
        guard !address.isEmpty else { return }
        connect(address: address, title: address, expectedFingerprint: nil)
    }

    private func connect(address: String, title: String, expectedFingerprint: String?, tunnel: PeerToPeerTunnel? = nil) {
        guard let core else { tunnel?.stop(); return }
        disconnectViewer()
        peerToPeerTunnel = tunnel
        switchOnStart = false
        holdingWindow = false
        let generation = viewerGeneration
        let window = ViewerWindowController(title: title)
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
            viewingByCode = false
        } catch {
            viewerStatus = "Couldn't connect: \(error.localizedDescription)"
        }
    }

    func disconnectViewer() {
        // This viewer's own sessionEnded is ignored from here on (see viewerGeneration), so reset
        // what it would have reset.
        viewerGeneration += 1
        // The window goes first: freeing the connection waits for the bye to go out.
        let w = viewerWindow
        viewerWindow = nil
        w?.onClose = nil
        w?.close()
        release(viewer)
        viewer = nil
        peerToPeerTunnel?.stop()
        peerToPeerTunnel = nil
        latency = nil
        controllingHost = false
        viewerStatus = nil
        onDirectWiFi = false
        holdingWindow = false
    }

    /// Frees a viewer connection off the main thread: it sends the bye and waits up to 300 ms for
    /// it to arrive, which would otherwise hold up closing the window.
    private func release(_ handle: ViewerHandle?) {
        guard let handle else { return }
        DispatchQueue.global(qos: .userInitiated).async { handle.disconnect() }
    }

    private func handleViewerEvent(_ e: CoreEvent) {
        switch e {
        case .sessionStarted(let s):
            let via = switch s.link {
            case "direct": " · cable"
            case "internet": " · internet"
            case "relayed": " · relay"
            default: ""
            }
            viewerStatus = (s.mode == "extend" ? "Extending \(s.peerName)" : "Viewing \(s.peerName)") + via
            viewerPeerName = s.peerName
            viewerFingerprint = s.fingerprint
            if directReconnect != nil {
                directReconnect = nil
                directStatus = nil
                onDirectWiFi = true
                holdingWindow = false
            }
            let route = switch s.link {
            case "direct": "Over a cable"
            case "internet": "Over the internet"
            case "relayed": "Through a relay"
            default: onDirectWiFi ? "On direct Wi-Fi" : ""
            }
            viewerWindow?.setPeer(s.peerName, detail: route)
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
            // The title set at session start keeps how it's reached (" · relay"); the size is a detail.
            viewerWindow?.setStreamSize(width: s.width, height: s.height)
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
                viewerStatus = reason == "user" ? nil : Self.friendlyReason(reason, peer: viewerPeerName, byCode: viewingByCode)
            }
            let w = viewerWindow
            viewerWindow = nil
            w?.onClose = nil
            w?.close()
            release(viewer)
            viewer = nil
            peerToPeerTunnel?.stop()
            peerToPeerTunnel = nil
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

    /// Joining a PC's hotspot by hand: for networks that keep computers from reaching each other,
    /// where no session can start to hand the hotspot over (the PC shows it under This PC → Direct Wi-Fi).
    @Published var pcHotspotName = "Annex-"
    @Published var pcHotspotPassword = ""
    @Published private(set) var joiningPCHotspot = false

    func joinPCHotspot() {
        let ssid = pcHotspotName.trimmingCharacters(in: .whitespaces)
        let passphrase = pcHotspotPassword.trimmingCharacters(in: .whitespaces)
        guard !ssid.isEmpty, !passphrase.isEmpty, !joiningPCHotspot else { return }
        joiningPCHotspot = true
        directStatus = "Joining “\(ssid)”…"
        DispatchQueue.global(qos: .userInitiated).async { [weak self] in
            let error = DirectWiFi.join(ssid: ssid, passphrase: passphrase)
            DispatchQueue.main.async {
                guard let self else { return }
                self.joiningPCHotspot = false
                if let error {
                    self.directStatus = "Couldn't join “\(ssid)”: \(error)"
                } else if self.isHosting {
                    // The PC finds this Mac on its hotspot and connects as usual.
                    self.hostDirectWiFi = ssid
                    self.directStatus = "On “\(ssid)”. Connect from the PC: this Mac shows up in its Nearby list."
                } else {
                    self.manualAddress = "\(DirectWiFi.windowsHotspotAddress):47800"
                    self.directStatus = "On “\(ssid)”. Show the PC's screen or extend it below."
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
        case .internetCode, .internetJoined, .internetRelay, .internetCodeEnded:
            handleInternetEvent(e)
        case .directLinkRequested(let session):
            host?.refuseDirectLink(session: session)
        case .directLinkUnavailable(_, let reason):
            // Host: the viewer couldn't start a hotspot.
            directStatus = reason
        case .inputRequested(_, let name, let fingerprint):
            NSApp.activate(ignoringOtherApps: true)
            let alert = NSAlert()
            alert.messageText = "Let \(name) control this Mac?"
            alert.informativeText = "\(name) wants to use this Mac's mouse and keyboard. You can change this in Paired Devices."
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
    static func friendlyReason(_ reason: String, peer: String, byCode: Bool = false) -> String {
        switch reason {
        case "extend_not_supported":
            return "\(peer) can't extend its display. Set up Extend on that computer, or use Show Its Screen."
        case "busy": return "\(peer) is already sharing with 4 computers."
        case "pairing_failed": return "Wrong PIN. Try again."
        case "pairing_cancelled": return "Pairing was cancelled on the other computer."
        case "pairing_locked": return "Too many wrong PINs. Try again in 10 minutes."
        case "host_stopped": return "\(peer) stopped sharing."
        case "host_ended_session": return "\(peer) ended the session."
        case "unknown_code": return "That code doesn't work. Codes work once, for 10 minutes: ask for a new one."
        case "too_many_attempts": return "Too many codes tried. Wait 10 minutes, then try again."
        case "code_expired": return "The code expired. Ask for a new one."
        case "closed", "connection closed": return "Disconnected from \(peer)."
        default:
            let r = reason.lowercased()
            if byCode && (r.contains("timed out") || r.contains("timeout")) {
                return "Couldn't connect across the internet. One of the networks blocks direct connections "
                    + "and no relay was available. Add your own relay in Settings → Internet on the sharing computer."
            }
            if byCode && (r.contains("rendezvous") || r.contains("i/o") || r.contains("websocket")) {
                return Self.friendlyInternetError(reason)
            }
            if r.contains("timed out") || r.contains("timeout") {
                // No answer at all: also what a network that keeps computers apart looks like.
                return "Couldn't reach \(peer). Check that it's sharing and on the same network. "
                    + "School and office Wi-Fi often blocks computers from reaching each other: "
                    + "for a Windows PC, use Direct Wi-Fi to a PC below."
            }
            if r.contains("refused") {
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
            lanPeers.removeAll { $0.instance == p.instance }
            lanPeers.append(p)
            rebuildNearby()
        case .peerLost(let instance):
            lanPeers.removeAll { $0.instance == instance }
            rebuildNearby()
        default:
            break
        }
    }

    /// The network's list, plus each Mac found only on peer-to-peer Wi-Fi (one row per computer:
    /// a Mac on both shows once, and connecting tries both ways).
    private func rebuildNearby() {
        let known = Set(lanPeers.compactMap(\.fingerprint))
        let peerOnly = peerToPeerHosts.filter { !known.contains($0.fingerprint) }.map(Self.peerInfo)
        discovered = (lanPeers + peerOnly).sorted { $0.name.localizedCaseInsensitiveCompare($1.name) == .orderedAscending }
        if case .peer(let instance) = pane, !discovered.contains(where: { $0.instance == instance }) { pane = .home }
    }

    nonisolated static func peerInfo(_ h: PeerToPeerHost) -> PeerInfo {
        PeerInfo(instance: "p2p:\(h.fingerprint)", name: h.name, addresses: [], port: 0, fingerprint: h.fingerprint,
                 protoMajor: 1, sharing: true, extend: h.extend)
    }

    /// Looks for Macs on peer-to-peer Wi-Fi only while Nearby or a computer's page is open, and not
    /// during a session: browsing keeps peer-to-peer Wi-Fi on, which takes some speed from the
    /// normal Wi-Fi link.
    private func updatePeerToPeerBrowsing(pane: Pane, on: Bool) {
        let wanted: Bool
        switch pane {
        case .home, .connect, .peer: wanted = on
        default: wanted = false
        }
        if wanted, peerToPeerBrowser == nil {
            peerToPeerBrowser = PeerToPeerBrowser(known: { [weak self] in
                Dictionary((self?.trusted ?? []).map { ($0.fingerprint, $0.name) }, uniquingKeysWith: { a, _ in a })
            }) { [weak self] hosts in
                guard let self else { return }
                let own = self.core?.fingerprint
                self.peerToPeerHosts = hosts.filter { $0.fingerprint != own }
                self.rebuildNearby()
            }
        } else if !wanted, let b = peerToPeerBrowser {
            b.stop()
            peerToPeerBrowser = nil
            peerToPeerHosts = []
            rebuildNearby()
        }
    }

    func quit() {
        disconnectViewer()
        stopHosting()
        NSApp.terminate(nil)
    }
}
