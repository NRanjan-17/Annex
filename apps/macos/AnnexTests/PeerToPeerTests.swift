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
                                                  visibility: .hidden)
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

    /// Paired Macs recognise a hidden-name advertisement; strangers don't, and it changes every
    /// quarter hour.
    func testOnlyPairedMacsRecogniseThePairedTag() {
        let fp = String(repeating: "c3", count: 32)
        let endpoint = NWEndpoint.hostPort(host: .ipv4(.loopback), port: 1)
        let now = Date(timeIntervalSince1970: 1_800_000_000)
        let e = PeerToPeer.epoch(now)
        let tag = PeerToPeer.tag(fingerprint: fp, epoch: e)
        let txt = ["v": "1", "t": tag, "role": "host", "sharing": "1", "extend": "1"]

        let paired = PeerToPeer.host(txt: txt, endpoint: endpoint, known: [fp: "Studio"], now: now)
        XCTAssertEqual(paired?.fingerprint, fp)
        XCTAssertEqual(paired?.name, "Studio")
        XCTAssertEqual(paired?.extend, true)
        XCTAssertNil(PeerToPeer.host(txt: txt, endpoint: endpoint, known: [:], now: now))
        XCTAssertNil(PeerToPeer.host(txt: txt, endpoint: endpoint, known: [String(repeating: "d4", count: 32): "Other"], now: now))
        // A clock a quarter hour off still matches; an hour off doesn't.
        XCTAssertNotNil(PeerToPeer.host(txt: txt, endpoint: endpoint, known: [fp: "Studio"], now: now.addingTimeInterval(900)))
        XCTAssertNil(PeerToPeer.host(txt: txt, endpoint: endpoint, known: [fp: "Studio"], now: now.addingTimeInterval(3600)))
        // The tag reveals nothing stable: a new one every quarter hour, never the fingerprint.
        XCTAssertNotEqual(tag, PeerToPeer.tag(fingerprint: fp, epoch: e + 1))
        XCTAssertFalse(fp.contains(tag))
        // Shown to everyone: name and fingerprint in the clear, a paired name preferred.
        let open = ["v": "1", "id": fp, "name": "Saidev’s Mac", "sharing": "1"]
        XCTAssertEqual(PeerToPeer.host(txt: open, endpoint: endpoint, known: [:], now: now)?.name, "Saidev’s Mac")
        XCTAssertEqual(PeerToPeer.host(txt: open, endpoint: endpoint, known: [fp: "Studio"], now: now)?.name, "Studio")
        XCTAssertNil(PeerToPeer.host(txt: ["v": "1", "id": fp, "sharing": "0"], endpoint: endpoint, known: [:], now: now))
    }
}
