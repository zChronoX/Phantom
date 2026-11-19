import Foundation

/// Archivia e sincronizza in `UserDefaults` i dispositivi Android accoppiati con il loro certificato fissato
/// e gli indirizzi LAN + Remoto (5G/4G · Tailscale o DDNS).
final class PairedDeviceStore: ObservableObject {
    @Published private(set) var devices: [PairedDevice] = []

    private let defaults: UserDefaults
    private let storageKey = "phantom.pairedDevices.v1"

    init(defaults: UserDefaults = .standard) {
        self.defaults = defaults
        load()
    }

    @discardableResult
    func upsert(from parsed: ParsedPairing) -> PairedDevice {
        if let idx = devices.firstIndex(where: { $0.id == parsed.fingerprintHex }) {
            var updated = devices[idx]
            updated.name = parsed.name
            if let lan = parsed.lanAddress {
                updated.lanAddress = lan
            }
            if let p2p = parsed.p2pEndpointId {
                updated.p2pEndpointId = p2p
            }
            if let wan = parsed.remoteAddress {
                updated.remoteAddress = wan
            }
            updated.certificateDer = parsed.certificateDer
            devices[idx] = updated
            save()
            return updated
        }

        let created = PairedDevice(
            name: parsed.name,
            lanAddress: parsed.lanAddress,
            p2pEndpointId: parsed.p2pEndpointId,
            remoteAddress: parsed.remoteAddress,
            certificateDer: parsed.certificateDer
        )
        devices.insert(created, at: 0)
        save()
        return created
    }

    func update(id: String, name: String? = nil, lanAddress: String?? = nil, remoteAddress: String?? = nil) {
        guard let idx = devices.firstIndex(where: { $0.id == id }) else { return }
        var d = devices[idx]
        if let name = name {
            let trimmed = name.trimmingCharacters(in: .whitespacesAndNewlines)
            if !trimmed.isEmpty { d.name = trimmed }
        }
        if let lanOpt = lanAddress {
            d.lanAddress = lanOpt.flatMap { EndpointParser.normalizeAddress($0) }
        }
        if let wanOpt = remoteAddress {
            d.remoteAddress = wanOpt.flatMap { EndpointParser.normalizeAddress($0) }
        }
        devices[idx] = d
        save()
    }

    func remove(id: String) {
        devices.removeAll { $0.id == id }
        save()
    }

    func remove(at offsets: IndexSet) {
        for idx in offsets.sorted(by: >) where idx < devices.count {
            devices.remove(at: idx)
        }
        save()
    }

    private func load() {
        guard let data = defaults.data(forKey: storageKey),
              let decoded = try? JSONDecoder().decode([PairedDevice].self, from: data) else {
            devices = []
            return
        }
        devices = decoded
    }

    private func save() {
        if let data = try? JSONEncoder().encode(devices) {
            defaults.set(data, forKey: storageKey)
        }
    }
}
