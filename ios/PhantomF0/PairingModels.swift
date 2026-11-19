import Foundation

/// Tipo di trasporto per un candidato di connessione verso l'host Android.
enum RemoteTransportKind: Equatable {
    /// QUIC diretto su IP/hostname (usato in LAN domestica o su indirizzo manuale).
    case directQuic(address: String, isLan: Bool)
    /// Trasporto P2P integrato (`iroh`: UDP hole punching + relay cifrato E2E, funziona ovunque in 5G/4G/Wi-Fi).
    case p2pIroh(endpointId: String)

    var displayAddress: String {
        switch self {
        case let .directQuic(address, _): return address
        case let .p2pIroh(endpointId): return "ID \(endpointId.prefix(12))…"
        }
    }
}

/// Candidato di connessione ordinato (es. prima LAN scoperta via Bonjour, poi LAN salvata, poi P2P integrato 5G/4G).
struct RemoteEndpointCandidate: Equatable {
    var target: RemoteTransportKind
    var label: String
    var timeoutMs: UInt64

    var displayAddress: String { target.displayAddress }
}

/// Dispositivo Android accoppiato e salvato su iPhone (supporta LAN domestica + P2P integrato 5G/4G senza VPN).
struct PairedDevice: Codable, Identifiable, Equatable {
    /// Prime 16 cifre esadecimali dello SHA-256 del certificato DER (identità stabile dell'host).
    var id: String
    var name: String
    var lanAddress: String?
    var p2pEndpointId: String?
    var remoteAddress: String?
    var certificateDer: Data
    var preferredCodec: String

    init(
        name: String,
        lanAddress: String?,
        p2pEndpointId: String? = nil,
        remoteAddress: String? = nil,
        certificateDer: Data,
        preferredCodec: String = "h265"
    ) {
        self.id = PhantomSHA256.fingerprintHex(certificateDer)
        self.name = name.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty ? "Android" : name
        self.lanAddress = lanAddress.flatMap { EndpointParser.normalizeAddress($0) }
        let cleanedP2p = p2pEndpointId?.trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
        self.p2pEndpointId = cleanedP2p.isEmpty ? nil : cleanedP2p
        self.remoteAddress = remoteAddress.flatMap { EndpointParser.normalizeAddress($0) }
        self.certificateDer = certificateDer
        self.preferredCodec = preferredCodec == "h264" ? "h264" : "h265"
    }
}

/// Dati estratti da un QR `phantom://pair?...`.
struct ParsedPairing: Equatable {
    var name: String
    var lanAddress: String?
    var p2pEndpointId: String?
    var remoteAddress: String?
    var certificateDer: Data
    var fingerprintHex: String
}

/// Codifica e decodifica dell'URI `phantom://pair?name=...&lan=...&p2p=...&wan=...&cert=...`.
enum PairingURI {
    static func parse(_ raw: String) -> ParsedPairing? {
        let trimmed = raw.trimmingCharacters(in: .whitespacesAndNewlines)
        guard trimmed.lowercased().hasPrefix("phantom://pair?"),
              let components = URLComponents(string: trimmed),
              let items = components.queryItems else {
            return nil
        }

        var map: [String: String] = [:]
        for item in items {
            if let value = item.value {
                map[item.name] = value
            }
        }

        guard let certRaw = map["cert"],
              let certDer = decodeBase64UrlOrStandard(certRaw),
              !certDer.isEmpty else {
            return nil
        }

        let name = (map["name"] ?? "").trimmingCharacters(in: .whitespacesAndNewlines)
        let lan = map["lan"].flatMap { EndpointParser.normalizeAddress($0) }
        let wan = map["wan"].flatMap { EndpointParser.normalizeAddress($0) }
        let p2p = (map["p2p"] ?? "").trimmingCharacters(in: .whitespacesAndNewlines)
        return ParsedPairing(
            name: name.isEmpty ? "Android" : name,
            lanAddress: lan,
            p2pEndpointId: p2p.isEmpty ? nil : p2p,
            remoteAddress: wan,
            certificateDer: certDer,
            fingerprintHex: PhantomSHA256.fingerprintHex(certDer)
        )
    }

    static func build(name: String, lan: String?, p2pId: String? = nil, wan: String? = nil, certDer: Data) -> String {
        var components = URLComponents()
        components.scheme = "phantom"
        components.host = "pair"
        var items: [URLQueryItem] = [
            URLQueryItem(name: "name", value: name.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty ? "Android" : name),
        ]
        if let lan = lan, !lan.isEmpty {
            items.append(URLQueryItem(name: "lan", value: lan))
        }
        if let p2pId = p2pId, !p2pId.isEmpty {
            items.append(URLQueryItem(name: "p2p", value: p2pId))
        }
        if let wan = wan, !wan.isEmpty {
            items.append(URLQueryItem(name: "wan", value: wan))
        }
        items.append(URLQueryItem(name: "cert", value: encodeBase64UrlNoPad(certDer)))
        components.queryItems = items
        return components.string ?? ""
    }

    static func encodeBase64UrlNoPad(_ data: Data) -> String {
        data.base64EncodedString()
            .replacingOccurrences(of: "+", with: "-")
            .replacingOccurrences(of: "/", with: "_")
            .replacingOccurrences(of: "=", with: "")
    }

    static func decodeBase64UrlOrStandard(_ raw: String) -> Data? {
        let trimmed = raw.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { return nil }
        var std = trimmed
            .replacingOccurrences(of: "-", with: "+")
            .replacingOccurrences(of: "_", with: "/")
        let rem = std.count % 4
        if rem == 1 { return nil }
        if rem > 0 {
            std.append(String(repeating: "=", count: 4 - rem))
        }
        if let data = Data(base64Encoded: std) {
            return data
        }
        return CertificateCodec.decode(trimmed)
    }
}

/// Costruisce la sequenza di trasporti da tentare: in Wi-Fi prova prima LAN diretta poi P2P integrato;
/// quando l'iPhone è solo in 5G/4G (`cellularOnly == true`) salta la LAN e va subito su P2P integrato.
enum EndpointCandidateSelector {
    static func candidates(
        for device: PairedDevice,
        discoveredLan: String?,
        cellularOnly: Bool = false
    ) -> [RemoteEndpointCandidate] {
        var out: [RemoteEndpointCandidate] = []
        var seenAddresses: Set<String> = []

        let hasP2p = device.p2pEndpointId.map { !$0.isEmpty } ?? false
        let normalizedRemote = device.remoteAddress.flatMap { EndpointParser.normalizeAddress($0) }
        let hasWideArea = hasP2p || normalizedRemote != nil

        func appendDirect(_ raw: String?, label: String, isLan: Bool, timeoutMs: UInt64) {
            guard let raw = raw, let norm = EndpointParser.normalizeAddress(raw), !seenAddresses.contains(norm) else {
                return
            }
            seenAddresses.insert(norm)
            out.append(RemoteEndpointCandidate(target: .directQuic(address: norm, isLan: isLan), label: label, timeoutMs: timeoutMs))
        }

        if !cellularOnly {
            appendDirect(discoveredLan, label: "LAN (trovato)", isLan: true, timeoutMs: hasWideArea ? 1_600 : 5_000)
            appendDirect(device.lanAddress, label: "LAN", isLan: true, timeoutMs: hasWideArea ? 1_300 : 5_000)
        }

        if let p2p = device.p2pEndpointId, !p2p.isEmpty {
            out.append(RemoteEndpointCandidate(target: .p2pIroh(endpointId: p2p), label: "P2P 5G/Ovunque", timeoutMs: 12_000))
        }

        if let remote = normalizedRemote {
            appendDirect(remote, label: "Remoto", isLan: false, timeoutMs: 5_000)
        }

        // Se in 5G non esistono candidati P2P/remoti, mantieni comunque la LAN come ripiego
        if out.isEmpty {
            appendDirect(discoveredLan, label: "LAN (trovato)", isLan: true, timeoutMs: 5_000)
            appendDirect(device.lanAddress, label: "LAN", isLan: true, timeoutMs: 5_000)
        }
        return out
    }
}

/// Politica di backoff per la riconnessione automatica (es. cambio Wi-Fi ↔ 5G/4G).
enum ReconnectBackoff {
    static let maxAttempts = 12

    static func delaySeconds(attempt: Int) -> Double {
        let table = [0.6, 1.5, 2.5, 4.0, 6.0, 8.0]
        let idx = max(0, min(attempt - 1, table.count - 1))
        return table[idx]
    }
}

/// Implementazione SHA-256 pura (solo `Foundation`, compatibile anche con `swift:5.10` Linux nei test unitari).
enum PhantomSHA256 {
    private static let k: [UInt32] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
        0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
        0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
        0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
        0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
        0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
        0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
    ]

    static func digest(_ data: Data) -> Data {
        var msg = [UInt8](data)
        let bitLen = UInt64(msg.count) * 8
        msg.append(0x80)
        while (msg.count % 64) != 56 {
            msg.append(0)
        }
        for shift in stride(from: 56, through: 0, by: -8) {
            msg.append(UInt8((bitLen >> UInt64(shift)) & 0xFF))
        }

        var h0: UInt32 = 0x6a09e667
        var h1: UInt32 = 0xbb67ae85
        var h2: UInt32 = 0x3c6ef372
        var h3: UInt32 = 0xa54ff53a
        var h4: UInt32 = 0x510e527f
        var h5: UInt32 = 0x9b05688c
        var h6: UInt32 = 0x1f83d9ab
        var h7: UInt32 = 0x5be0cd19

        var w = [UInt32](repeating: 0, count: 64)
        for chunkStart in stride(from: 0, to: msg.count, by: 64) {
            for i in 0..<16 {
                let j = chunkStart + i * 4
                w[i] = (UInt32(msg[j]) << 24)
                    | (UInt32(msg[j + 1]) << 16)
                    | (UInt32(msg[j + 2]) << 8)
                    | UInt32(msg[j + 3])
            }
            for i in 16..<64 {
                let s0 = ror(w[i - 15], 7) ^ ror(w[i - 15], 18) ^ (w[i - 15] >> 3)
                let s1 = ror(w[i - 2], 17) ^ ror(w[i - 2], 19) ^ (w[i - 2] >> 10)
                w[i] = w[i - 16] &+ s0 &+ w[i - 7] &+ s1
            }

            var a = h0, b = h1, c = h2, d = h3
            var e = h4, f = h5, g = h6, h = h7

            for i in 0..<64 {
                let s1 = ror(e, 6) ^ ror(e, 11) ^ ror(e, 25)
                let ch = (e & f) ^ ((~e) & g)
                let temp1 = h &+ s1 &+ ch &+ k[i] &+ w[i]
                let s0 = ror(a, 2) ^ ror(a, 13) ^ ror(a, 22)
                let maj = (a & b) ^ (a & c) ^ (b & c)
                let temp2 = s0 &+ maj

                h = g
                g = f
                f = e
                e = d &+ temp1
                d = c
                c = b
                b = a
                a = temp1 &+ temp2
            }

            h0 = h0 &+ a
            h1 = h1 &+ b
            h2 = h2 &+ c
            h3 = h3 &+ d
            h4 = h4 &+ e
            h5 = h5 &+ f
            h6 = h6 &+ g
            h7 = h7 &+ h
        }

        var out = Data(capacity: 32)
        for v in [h0, h1, h2, h3, h4, h5, h6, h7] {
            out.append(UInt8((v >> 24) & 0xFF))
            out.append(UInt8((v >> 16) & 0xFF))
            out.append(UInt8((v >> 8) & 0xFF))
            out.append(UInt8(v & 0xFF))
        }
        return out
    }

    static func fingerprintHex(_ data: Data, length: Int = 16) -> String {
        let hex = CertificateCodec.encodeHex(digest(data))
        return String(hex.prefix(max(2, min(length, hex.count))))
    }

    private static func ror(_ x: UInt32, _ n: UInt32) -> UInt32 {
        (x >> n) | (x << (32 - n))
    }
}
