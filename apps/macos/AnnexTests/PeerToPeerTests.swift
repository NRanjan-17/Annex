@testable import Annex
import Network
import XCTest

/// The peer-to-peer relay on one Mac: viewer tunnel → host relay → a stand-in for the core's QUIC
/// port, and back. Bonjour browsing and the AWDL radio itself need two signed Macs.
final class PeerToPeerTests: XCTestCase {
    func testDatagramsCrossTheRelayBothWays() throws {
        // The "core": echoes every datagram.
        let queue = DispatchQueue(label: "test.echo")
        let echo = try NWListener(using: .udp)
        echo.newConnectionHandler = { c in
            c.start(queue: queue)
            func loop() {
                c.receiveMessage { data, _, _, error in
                    if let data { c.send(content: data, completion: .idempotent) }
                    if error == nil { loop() }
                }
            }
            loop()
        }
        let echoReady = expectation(description: "echo ready")
        echo.stateUpdateHandler = { if case .ready = $0 { echoReady.fulfill() } }
        echo.start(queue: queue)
        wait(for: [echoReady], timeout: 5)
        let corePort = try XCTUnwrap(echo.port?.rawValue)

        let fingerprint = String(repeating: "ab", count: 32)
        let name = "Annex Test \(UUID().uuidString.prefix(6))"
        let advertiser = try PeerToPeerAdvertiser(name: name, fingerprint: fingerprint, extend: true, corePort: corePort,
                                                  advertise: false)
        defer { advertiser.stop() }

        // Bonjour needs the Local Network permission, which an ad-hoc signed test build doesn't
        // get (NoAuth), so the tunnel goes to the relay's port directly.
        let deadline = Date().addingTimeInterval(5)
        while (advertiser.port ?? 0) == 0 && Date() < deadline { RunLoop.current.run(until: Date().addingTimeInterval(0.05)) }
        let relayPort = try XCTUnwrap(advertiser.port.flatMap { NWEndpoint.Port(rawValue: $0) })
        let endpoint = NWEndpoint.hostPort(host: .ipv4(.loopback), port: relayPort)

        let ready = expectation(description: "tunnel ready")
        var local: String?
        let tunnel = try PeerToPeerTunnel(to: endpoint) { local = $0; ready.fulfill() }
        defer { tunnel.stop() }
        wait(for: [ready], timeout: 5)
        let address = try XCTUnwrap(local)
        let port = try XCTUnwrap(UInt16(address.split(separator: ":").last!))

        let client = NWConnection(host: "127.0.0.1", port: NWEndpoint.Port(rawValue: port)!, using: .udp)
        client.start(queue: queue)
        let back = expectation(description: "datagram comes back")
        let payload = Data("hello over the relay".utf8)
        client.send(content: payload, completion: .idempotent)
        client.receiveMessage { data, _, _, _ in
            XCTAssertEqual(data, payload)
            back.fulfill()
        }
        wait(for: [back], timeout: 10)
        client.cancel()
        echo.cancel()
    }
}
