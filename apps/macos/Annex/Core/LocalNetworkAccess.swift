import Foundation
import Network

/// Asks macOS for Local Network access as soon as Annex opens.
///
/// Annex's discovery and connections use the core's own sockets, and macOS may not ask about those
/// until this Mac reaches out to another computer itself. Until it asks (and the user allows it),
/// this Mac can't answer a viewer that connects to it, so sharing from the Mac looks broken until
/// it has viewed another computer once. A short Bonjour browse through Network.framework makes
/// macOS show the prompt right away instead.
@MainActor
final class LocalNetworkAccess {
    private var browser: NWBrowser?

    func request() {
        guard browser == nil else { return }
        let parameters = NWParameters()
        parameters.includePeerToPeer = false
        let b = NWBrowser(for: .bonjour(type: "_annex._tcp", domain: nil), using: parameters)
        b.browseResultsChangedHandler = { _, _ in }
        b.start(queue: .main)
        browser = b
        // The prompt only needs the browse to start; stop after a moment.
        DispatchQueue.main.asyncAfter(deadline: .now() + 10) { [weak self] in
            self?.browser?.cancel()
            self?.browser = nil
        }
    }
}
