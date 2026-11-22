import Foundation
import Network
import Darwin

/// Host Android individuato automaticamente sulla rete locale tramite Bonjour (`_phantom._udp`).
struct DiscoveredLanHost: Identifiable, Equatable {
    var id: String
    var serviceName: String
    var displayName: String
    var fingerprintHex: String?
    var address: String?
}

/// Scopre gli host Phantom nella rete locale tramite Bonjour (`_phantom._udp.`) e ne risolve IPv4 e porta
/// così l'iPhone si collega automaticamente in LAN quando sei a casa senza scrivere IP a mano.
final class BonjourDiscovery: NSObject, ObservableObject {
    @Published private(set) var hosts: [DiscoveredLanHost] = []

    private var browser: NWBrowser?
    private let queue = DispatchQueue(label: "app.phantom.bonjour")
    private var resolvers: [String: NetService] = [:]

    func start() {
        guard browser == nil else { return }
        let params = NWParameters.udp
        // Serve solo la LAN. Con `includePeerToPeer = true` iOS accende AWDL (la radio peer-to-peer di AirDrop):
        // il Wi-Fi salta periodicamente su un altro canale e lo streaming ha picchi di latenza e cali di banda a
        // intervalli regolari (lo stesso problema noto a Moonlight, Steam Link e Parsec).
        params.includePeerToPeer = false
        let descriptor = NWBrowser.Descriptor.bonjourWithTXTRecord(type: "_phantom._udp", domain: nil)
        let b = NWBrowser(for: descriptor, using: params)
        b.browseResultsChangedHandler = { [weak self] results, _ in
            self?.handleBrowseResults(results)
        }
        b.start(queue: queue)
        browser = b
    }

    func stop() {
        browser?.cancel()
        browser = nil
        DispatchQueue.main.async {
            for svc in self.resolvers.values {
                svc.stop()
            }
            self.resolvers.removeAll()
        }
    }

    func resolvedAddress(forFingerprint fingerprintHex: String) -> String? {
        hosts.first(where: { $0.fingerprintHex == fingerprintHex })?.address
    }

    private func handleBrowseResults(_ results: Set<NWBrowser.Result>) {
        var next: [DiscoveredLanHost] = []
        var toResolve: [(key: String, name: String, type: String, domain: String)] = []

        for result in results {
            guard case let .service(name, type, domain, _) = result.endpoint else { continue }
            let key = "\(name)|\(type)|\(domain)"
            var fp: String?
            var displayName = name
            if case let .bonjour(txt) = result.metadata {
                if let f = txt["fp"], !f.isEmpty { fp = f }
                if let n = txt["name"], !n.isEmpty { displayName = n }
            }
            next.append(DiscoveredLanHost(
                id: key,
                serviceName: name,
                displayName: displayName,
                fingerprintHex: fp,
                address: nil
            ))
            toResolve.append((key, name, type, domain))
        }

        DispatchQueue.main.async {
            let existingAddr = Dictionary(uniqueKeysWithValues: self.hosts.compactMap { h in
                h.address.map { (h.id, $0) }
            })
            self.hosts = next.map { item in
                var copy = item
                copy.address = existingAddr[item.id]
                return copy
            }

            let activeKeys = Set(toResolve.map { $0.key })
            for (k, svc) in self.resolvers where !activeKeys.contains(k) {
                svc.stop()
                self.resolvers.removeValue(forKey: k)
            }
            for item in toResolve where self.resolvers[item.key] == nil {
                let svc = NetService(domain: item.domain, type: item.type, name: item.name)
                svc.delegate = self
                self.resolvers[item.key] = svc
                svc.resolve(withTimeout: 4.0)
            }
        }
    }
}

extension BonjourDiscovery: NetServiceDelegate {
    func netServiceDidResolveAddress(_ sender: NetService) {
        guard let ip = Self.extractIpv4(from: sender.addresses), sender.port > 0 else { return }
        let addr = "\(ip):\(sender.port)"
        let matchingKey = resolvers.first(where: { $0.value === sender })?.key
        DispatchQueue.main.async {
            guard let key = matchingKey,
                  let idx = self.hosts.firstIndex(where: { $0.id == key }) else { return }
            self.hosts[idx].address = addr
        }
    }

    private static func extractIpv4(from addresses: [Data]?) -> String? {
        guard let addresses = addresses else { return nil }
        for data in addresses {
            var ip: String?
            data.withUnsafeBytes { raw in
                guard let base = raw.baseAddress else { return }
                let sa = base.assumingMemoryBound(to: sockaddr.self)
                if sa.pointee.sa_family == sa_family_t(AF_INET) {
                    var addr = base.assumingMemoryBound(to: sockaddr_in.self).pointee.sin_addr
                    var buf = [CChar](repeating: 0, count: Int(INET_ADDRSTRLEN))
                    if inet_ntop(AF_INET, &addr, &buf, socklen_t(INET_ADDRSTRLEN)) != nil {
                        ip = String(cString: buf)
                    }
                }
            }
            if let found = ip, !found.hasPrefix("127."), !found.hasPrefix("169.254.") {
                return found
            }
        }
        return nil
    }
}
