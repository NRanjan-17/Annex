import SwiftUI

struct SettingsView: View {
    @ObservedObject var model: AppModel
    @State private var pane: Pane? = .general

    enum Pane: String, CaseIterable, Identifiable {
        case general, sharing, viewing, devices, about
        var id: String { rawValue }

        var title: String {
            switch self {
            case .general: "General"
            case .sharing: "Sharing"
            case .viewing: "Viewing"
            case .devices: "Devices"
            case .about: "About"
            }
        }

        var symbol: String {
            switch self {
            case .general: "gearshape.fill"
            case .sharing: "display.2"
            case .viewing: "play.rectangle.fill"
            case .devices: "laptopcomputer.and.iphone"
            case .about: "info.circle.fill"
            }
        }

        /// System Settings-style coloured tiles.
        var tint: Color {
            switch self {
            case .general: .gray
            case .sharing: .blue
            case .viewing: .indigo
            case .devices: .green
            case .about: .secondary
            }
        }
    }

    var body: some View {
        NavigationSplitView {
            List(Pane.allCases, selection: $pane) { p in
                Label {
                    Text(p.title)
                } icon: {
                    Image(systemName: p.symbol)
                        .font(.system(size: 11, weight: .semibold))
                        .foregroundStyle(.white)
                        .frame(width: 22, height: 22)
                        .background(RoundedRectangle(cornerRadius: 6, style: .continuous).fill(p.tint.gradient))
                }
                .tag(p)
            }
            .navigationSplitViewColumnWidth(190)
            .toolbar(removing: .sidebarToggle)
        } detail: {
            Group {
                switch pane ?? .general {
                case .general: GeneralSettings(settings: model.settings)
                case .sharing: SharingSettings(model: model, settings: model.settings)
                case .viewing: ViewingSettings(settings: model.settings)
                case .devices: DevicesSettings(model: model)
                case .about: AboutSettings(model: model)
                }
            }
            .navigationTitle((pane ?? .general).title)
        }
        .frame(width: 720, height: 500)
        .onAppear {
            model.refreshTrusted()
            model.refreshPermissions()
        }
    }
}

private struct GeneralSettings: View {
    @ObservedObject var settings: AppSettings

    var body: some View {
        Form {
            Section {
                Toggle("Open Annex at login", isOn: Binding(get: { settings.launchAtLogin }, set: { settings.setLaunchAtLogin($0) }))
                if let err = settings.launchAtLoginError {
                    Text(err).font(.caption).foregroundStyle(.red)
                }
                Toggle("Start sharing this Mac when Annex opens", isOn: $settings.startSharingAtLaunch)
            } header: {
                Text("Startup")
            } footer: {
                Text("Annex lives in the menu bar. Everything stays on your network: no accounts, no cloud.")
                    .font(.caption).foregroundStyle(.secondary)
            }
        }
        .formStyle(.grouped)
    }
}

private struct SharingSettings: View {
    @ObservedObject var model: AppModel
    @ObservedObject var settings: AppSettings

    var body: some View {
        Form {
            Section {
                Toggle("Share this Mac", isOn: Binding(get: { model.isHosting }, set: { model.setHosting($0) }))
                if model.displays.count > 1 {
                    Picker("Display to mirror", selection: $model.selectedDisplayID) {
                        ForEach(model.displays) { d in Text(d.name).tag(Optional(d.id)) }
                    }
                    .disabled(model.isHosting)
                }
            } footer: {
                Text("Other computers on this network can mirror this screen, or use themselves as an extra display for it. New devices need the PIN this Mac shows.")
                    .font(.caption).foregroundStyle(.secondary)
            }
            Section {
                Toggle(isOn: $settings.preferHardwareEncoder) {
                    Text("Use the hardware video encoder")
                    Text("Recommended. The software encoder adds about a quarter of a second of delay.")
                }
                .disabled(model.isHosting)
            } header: {
                Text("Video")
            } footer: {
                if model.isHosting { Text("Stop sharing to change the encoder.").font(.caption).foregroundStyle(.secondary) }
            }
        }
        .formStyle(.grouped)
    }
}

private struct ViewingSettings: View {
    @ObservedObject var settings: AppSettings

    var body: some View {
        Form {
            Section("Window") {
                Toggle("Open in full screen", isOn: $settings.openFullScreen)
                Toggle("Use as an extra display by default", isOn: $settings.extendByDefault)
            }
            Section {
                Toggle(isOn: $settings.controlWhenViewing) {
                    Text("Control the other computer with this Mac's mouse and keyboard")
                    Text("The other computer must allow this Mac in its Devices settings.")
                }
            } header: {
                Text("Control")
            } footer: {
                Text("While you're in control, press ⌃⌥⇧Q to leave the session.")
                    .font(.caption).foregroundStyle(.secondary)
            }
        }
        .formStyle(.grouped)
    }
}

private struct DevicesSettings: View {
    @ObservedObject var model: AppModel

    var body: some View {
        Form {
            if model.trusted.contains(where: \.allowInput) && !model.accessibilityTrusted {
                Section {
                    HStack(alignment: .top, spacing: 10) {
                        Image(systemName: "hand.raised.fill").foregroundStyle(.orange).font(.title3)
                        VStack(alignment: .leading, spacing: 6) {
                            Text("Annex needs Accessibility access").font(.headline)
                            Text("macOS only lets apps use your mouse and keyboard with this permission. Turn on Annex in Privacy & Security → Accessibility.")
                                .font(.callout).foregroundStyle(.secondary)
                            Button("Open Accessibility Settings") { InputInjector.openAccessibilitySettings() }
                        }
                    }
                    .padding(.vertical, 4)
                }
            }
            Section {
                if model.trusted.isEmpty {
                    VStack(spacing: 8) {
                        Image(systemName: "lock.shield").font(.system(size: 28)).foregroundStyle(.tertiary)
                        Text("No paired devices yet").font(.headline)
                        Text("Connect from another computer and enter the PIN this Mac shows. Paired devices appear here.")
                            .font(.callout).foregroundStyle(.secondary).multilineTextAlignment(.center)
                    }
                    .frame(maxWidth: .infinity)
                    .padding(.vertical, 24)
                } else {
                    ForEach(model.trusted) { peer in DeviceRow(model: model, peer: peer) }
                }
            } header: {
                Text("Paired devices")
            } footer: {
                Text("Paired devices connect without a PIN. Remove one to require pairing again.")
                    .font(.caption).foregroundStyle(.secondary)
            }
        }
        .formStyle(.grouped)
    }
}

private struct DeviceRow: View {
    @ObservedObject var model: AppModel
    let peer: TrustedPeer
    @State private var confirmRemove = false

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack(spacing: 10) {
                Image(systemName: "desktopcomputer")
                    .font(.system(size: 16))
                    .foregroundStyle(Color.accentColor)
                    .frame(width: 30, height: 30)
                    .background(Circle().fill(Color.accentColor.opacity(0.12)))
                VStack(alignment: .leading, spacing: 1) {
                    Text(peer.name).font(.system(size: 13, weight: .medium))
                    Text(lastSeen).font(.caption).foregroundStyle(.secondary)
                }
                Spacer()
                Button(role: .destructive) { confirmRemove = true } label: { Text("Remove") }
                    .controlSize(.small)
            }
            Toggle(isOn: Binding(get: { peer.allowInput }, set: { model.setAllowInput(peer, $0) })) {
                Text("Can control this Mac's mouse and keyboard")
            }
            .toggleStyle(.switch)
            .controlSize(.small)
            Text(Annex.displayFingerprint(peer.fingerprint))
                .font(.system(size: 10.5, design: .monospaced))
                .foregroundStyle(.tertiary)
                .textSelection(.enabled)
        }
        .padding(.vertical, 4)
        .confirmationDialog("Remove \(peer.name)?", isPresented: $confirmRemove) {
            Button("Remove", role: .destructive) { model.revoke(peer) }
        } message: {
            Text("It will need a PIN to connect again.")
        }
    }

    private var lastSeen: String {
        guard peer.lastSeen > 0 else { return "Paired" }
        let date = Date(timeIntervalSince1970: TimeInterval(peer.lastSeen))
        return "Last connected " + date.formatted(.relative(presentation: .named))
    }
}

private struct AboutSettings: View {
    @ObservedObject var model: AppModel

    var body: some View {
        VStack(spacing: 12) {
            Spacer()
            Image(nsImage: NSApp.applicationIconImage)
                .resizable()
                .frame(width: 96, height: 96)
            Text("Annex").font(.system(size: 22, weight: .semibold))
            Text("Version \(Annex.version)").foregroundStyle(.secondary)
            Text("Use another computer as a second display, or mirror its screen — over your network or a cable. Encrypted end to end, no accounts, no cloud.")
                .multilineTextAlignment(.center)
                .font(.callout)
                .foregroundStyle(.secondary)
                .frame(maxWidth: 360)
            HStack(spacing: 16) {
                Link("Website", destination: URL(string: "https://github.com/NRanjan-17/Annex")!)
                Link("Report an issue", destination: URL(string: "https://github.com/NRanjan-17/Annex/issues")!)
            }
            .font(.callout)
            Spacer()
            Text("MIT License · Copyright © 2026 Annex contributors")
                .font(.caption).foregroundStyle(.tertiary)
                .padding(.bottom, 16)
        }
        .frame(maxWidth: .infinity)
    }
}
