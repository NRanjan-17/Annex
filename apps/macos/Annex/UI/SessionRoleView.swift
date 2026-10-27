import SwiftUI

extension SessionRole {
    /// "This Mac is sharing · Extend": the role first, so it never depends on the other name.
    var headline: String {
        switch self {
        case .sharing(_, let extend): "This Mac is sharing · \(extend ? "Extend" : "Mirror")"
        case .viewing(_, let extend): "This Mac is viewing · \(extend ? "Extend" : "Mirror")"
        }
    }

    var peerName: String {
        switch self { case .sharing(let n, _), .viewing(let n, _): n }
    }
}

/// [This Mac] ──▶ [Other]: the computer that shares on the left, the one that shows it on the right.
struct RoleFlow: View {
    @ObservedObject var model: AppModel
    let role: SessionRole
    var compact = false

    var body: some View {
        HStack(spacing: compact ? 6 : 10) {
            switch role {
            case .sharing(let name, let extend):
                end("This Mac", caption: extend ? "adds a display" : "sharing", thisMac: true)
                arrow
                end(model.label(name), caption: "shows it", thisMac: false)
            case .viewing(let name, let extend):
                end(model.label(name), caption: extend ? "adds a display" : "sharing", thisMac: false)
                arrow
                end("This Mac", caption: "shows it", thisMac: true)
            }
        }
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(accessibility)
    }

    private var arrow: some View {
        Image(systemName: "arrow.right")
            .font(.system(size: compact ? 11 : 14, weight: .bold))
            .foregroundStyle(Color.annexLive)
    }

    private func end(_ name: String, caption: String, thisMac: Bool) -> some View {
        VStack(alignment: .leading, spacing: 2) {
            HStack(spacing: 5) {
                Image(systemName: thisMac ? "laptopcomputer" : "display")
                    .font(.system(size: compact ? 10 : 12, weight: .semibold))
                Text(name)
                    .font(.system(size: compact ? 11.5 : 13, weight: .semibold))
                    .lineLimit(1)
                    .truncationMode(.middle)
            }
            .padding(.horizontal, compact ? 7 : 10)
            .padding(.vertical, compact ? 3 : 5)
            .foregroundStyle(thisMac ? Color.black : Color.primary)
            .background(Capsule().fill(thisMac ? Color.white.opacity(0.92) : Color.white.opacity(0.08)))
            Text(caption)
                .font(.system(size: compact ? 10 : 11))
                .foregroundStyle(.secondary)
                .padding(.leading, compact ? 7 : 10)
        }
        .layoutPriority(thisMac ? 1 : 0)
    }

    private var accessibility: String {
        switch role {
        case .sharing(let name, let extend):
            extend ? "This Mac adds a display that shows on \(model.label(name))"
                   : "This Mac's screen shows on \(model.label(name))"
        case .viewing(let name, let extend):
            extend ? "This Mac shows an extra display from \(model.label(name))"
                   : "This Mac shows \(model.label(name))'s screen"
        }
    }
}

/// Both computers have this Mac's name, so nothing on screen can say which is which.
struct SameNameNotice: View {
    @ObservedObject var model: AppModel
    var compact = false
    let rename: () -> Void

    var body: some View {
        if let name = model.sameNamePeer {
            HStack(alignment: .center, spacing: 10) {
                Image(systemName: "exclamationmark.triangle.fill")
                    .foregroundStyle(.orange)
                VStack(alignment: .leading, spacing: 2) {
                    Text("Both computers are called “\(name)”.")
                        .font(.system(size: compact ? 11.5 : 13, weight: .semibold))
                        .fixedSize(horizontal: false, vertical: true)
                    Text("Give this Mac its own name.")
                        .font(.system(size: compact ? 11 : 12))
                        .foregroundStyle(.secondary)
                }
                Spacer(minLength: 0)
                Button("Rename…", action: rename)
                    .pointingHand()
                    .controlSize(compact ? .small : .regular)
            }
            .padding(compact ? 10 : 14)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(RoundedRectangle(cornerRadius: compact ? 12 : 16, style: .continuous).fill(Color.orange.opacity(0.10)))
            .overlay(RoundedRectangle(cornerRadius: compact ? 12 : 16, style: .continuous).strokeBorder(Color.orange.opacity(0.30)))
        }
    }
}
