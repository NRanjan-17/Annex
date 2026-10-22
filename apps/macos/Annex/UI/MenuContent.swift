import SwiftUI

struct MenuContent: View {
    @ObservedObject var model: AppModel

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            header
            if let err = model.startupError {
                Text(err).foregroundStyle(.red).font(.callout)
            }
            Divider()
            hostSection
            Divider()
            viewSection
            if !model.trusted.isEmpty {
                Divider()
                trustedSection
            }
            Divider()
            HStack {
                Text("Annex \(Annex.version)").font(.caption).foregroundStyle(.secondary)
                Spacer()
                Button("Quit") { model.quit() }.keyboardShortcut("q")
            }
        }
        .padding(14)
        .frame(width: 340)
    }

    private var header: some View {
        VStack(alignment: .leading, spacing: 2) {
            Text(model.deviceName).font(.headline)
            Text(model.fingerprintDisplay)
                .font(.system(.caption, design: .monospaced))
                .foregroundStyle(.secondary)
                .textSelection(.enabled)
                .help("This device's identity code. Compare it with what the other device shows when pairing.")
        }
    }

    private var hostSection: some View {
        VStack(alignment: .leading, spacing: 6) {
            Toggle("Share this screen (Mirror)", isOn: Binding(get: { model.isHosting }, set: { model.setHosting($0) }))
                .toggleStyle(.switch)
            if model.displays.count > 1 {
                Picker("Display", selection: $model.selectedDisplayID) {
                    ForEach(model.displays) { d in Text(d.name).tag(Optional(d.id)) }
                }
                .disabled(model.isHosting)
            }
            Toggle("Hardware encoder", isOn: $model.preferHardwareEncoder)
                .disabled(model.isHosting)
                .font(.callout)
                .help("Off forces Apple's software encoder, which adds about 280 ms of latency.")
            if model.isHosting {
                let s = model.hostStatus
                if let viewer = s.viewerName, let st = s.stream {
                    Label("Streaming to \(viewer) — \(st.width)×\(st.height) @ \(st.fps)\(s.hardwareEncoder ? " (HW)" : " (SW)")",
                          systemImage: "dot.radiowaves.left.and.right")
                        .font(.callout)
                } else if let port = s.listeningPort {
                    Label("Waiting for a viewer on port \(port)…", systemImage: "hourglass").font(.callout)
                }
            }
            if let err = model.hostStatus.error {
                Text(err).foregroundStyle(.red).font(.caption)
            }
        }
    }

    private var viewSection: some View {
        VStack(alignment: .leading, spacing: 6) {
            Text("View another computer").font(.subheadline.bold())
            if model.discovered.isEmpty {
                Text("Looking for hosts on this network…").font(.callout).foregroundStyle(.secondary)
            }
            ForEach(model.discovered) { peer in
                Button {
                    model.connect(to: peer)
                } label: {
                    Label(peer.name, systemImage: "display")
                }
                .buttonStyle(.link)
            }
            HStack {
                TextField("IP address or host:port", text: $model.manualAddress)
                    .textFieldStyle(.roundedBorder)
                    .onSubmit { model.connectManual() }
                Button("Connect") { model.connectManual() }
            }
            if let s = model.viewerStatus {
                Text(s).font(.callout)
            }
            if let l = model.latency {
                Text(String(format: "Latency p50 %.0f ms · p95 %.0f ms · %.0f fps", l.p50Ms, l.p95Ms, l.fps))
                    .font(.caption).foregroundStyle(.secondary)
            }
        }
    }

    private var trustedSection: some View {
        VStack(alignment: .leading, spacing: 4) {
            Text("Trusted devices").font(.subheadline.bold())
            ForEach(model.trusted) { peer in
                HStack {
                    Text(peer.name).lineLimit(1)
                    Spacer()
                    Button("Remove") { model.revoke(peer) }
                        .buttonStyle(.borderless)
                        .help("Revoke trust. The device will need approval to connect again.")
                }
                .font(.callout)
            }
        }
    }
}
