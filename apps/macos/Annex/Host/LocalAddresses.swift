import Darwin
import Foundation

/// This Mac's IP addresses, for telling the user what to type on the other computer.
enum LocalAddresses {
    struct Address: Hashable {
        let ip: String
        /// Interface name, e.g. en0.
        let interface: String
        /// 169.254.x.x / fe80::: a cable straight between the two computers.
        let isDirectLink: Bool
    }

    /// IPv4 addresses (easier to type) of interfaces that are up; LAN addresses first.
    static func current() -> [Address] {
        var list: UnsafeMutablePointer<ifaddrs>?
        guard getifaddrs(&list) == 0, let first = list else { return [] }
        defer { freeifaddrs(list) }
        var out: [Address] = []
        for ptr in sequence(first: first, next: { $0.pointee.ifa_next }) {
            let ifa = ptr.pointee
            let flags = Int32(ifa.ifa_flags)
            guard let sa = ifa.ifa_addr, sa.pointee.sa_family == UInt8(AF_INET),
                  flags & IFF_UP != 0, flags & IFF_LOOPBACK == 0 else { continue }
            var host = [CChar](repeating: 0, count: Int(NI_MAXHOST))
            guard getnameinfo(sa, socklen_t(sa.pointee.sa_len), &host, socklen_t(host.count), nil, 0, NI_NUMERICHOST) == 0 else { continue }
            let ip = String(cString: host)
            let name = String(cString: ifa.ifa_name)
            // Skip VPN/tunnel and virtual bridge interfaces nobody would type.
            if name.hasPrefix("utun") || name.hasPrefix("bridge") || name.hasPrefix("awdl") || name.hasPrefix("llw") { continue }
            out.append(Address(ip: ip, interface: name, isDirectLink: ip.hasPrefix("169.254.")))
        }
        return out.sorted { ($0.isDirectLink ? 1 : 0, $0.interface) < ($1.isDirectLink ? 1 : 0, $1.interface) }
    }
}
