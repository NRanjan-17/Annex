import Foundation

/// Joins the hotspot another computer runs for a direct link (one Wi-Fi hop, no router), with
/// macOS's own networksetup tool. When the hotspot goes away, macOS rejoins the usual network.
enum DirectWiFi {
    /// Windows' hotspot always gives the PC this address.
    static let windowsHotspotAddress = "192.168.137.1"

    /// The Wi-Fi interface (usually en0), from `networksetup -listallhardwareports`.
    static func wifiInterface() -> String? {
        guard let out = run("/usr/sbin/networksetup", ["-listallhardwareports"]).output else { return nil }
        var sawWiFi = false
        for line in out.split(separator: "\n") {
            if line.hasPrefix("Hardware Port:") { sawWiFi = line.contains("Wi-Fi") || line.contains("AirPort") }
            if sawWiFi, line.hasPrefix("Device:") { return line.replacingOccurrences(of: "Device:", with: "").trimmingCharacters(in: .whitespaces) }
        }
        return nil
    }

    /// Joins `ssid`. Returns nil on success, or what went wrong. Blocks for a few seconds: call off the main thread.
    static func join(ssid: String, passphrase: String) -> String? {
        guard let iface = wifiInterface() else {
            Annex.log(.warn, "[network] no Wi-Fi interface found")
            return "This Mac has no Wi-Fi."
        }
        // The network can take a moment to appear after the hotspot starts.
        var last = ""
        for attempt in 0..<4 {
            let r = run("/usr/sbin/networksetup", ["-setairportnetwork", iface, ssid, passphrase])
            let text = (r.output ?? "").trimmingCharacters(in: .whitespacesAndNewlines)
            if r.status == 0 && text.isEmpty {
                Annex.log(.debug, "[network] joined \"\(ssid)\" on \(iface) (try \(attempt + 1))")
                return nil
            }
            last = text.isEmpty ? "networksetup exited with \(r.status)" : text
            Annex.log(.debug, "[network] joining \"\(ssid)\" on \(iface), try \(attempt + 1) of 4: \(redact(last, passphrase))")
            Thread.sleep(forTimeInterval: 2)
        }
        return last
    }

    /// `text` with the passphrase taken out, for the log.
    static func redact(_ text: String, _ passphrase: String) -> String {
        passphrase.isEmpty ? text : text.replacingOccurrences(of: passphrase, with: "(passphrase)")
    }

    private static func run(_ path: String, _ args: [String]) -> (status: Int32, output: String?) {
        let p = Process()
        p.executableURL = URL(fileURLWithPath: path)
        p.arguments = args
        let pipe = Pipe()
        p.standardOutput = pipe
        p.standardError = pipe
        do { try p.run() } catch { return (-1, error.localizedDescription) }
        p.waitUntilExit()
        let data = pipe.fileHandleForReading.readDataToEndOfFile()
        return (p.terminationStatus, String(data: data, encoding: .utf8))
    }
}
