import Foundation

/// Rettangolo del video renderizzato dentro una vista (`AVLayerVideoGravity.resizeAspect`).
///
/// Tutte le strutture in questo file usano solo `Foundation` così sono verificabili con `swiftc` in `ios/Tests`.
struct VideoRect: Equatable {
    var x: Double
    var y: Double
    var width: Double
    var height: Double

    var isEmpty: Bool { width <= 0 || height <= 0 }

    func contains(x px: Double, y py: Double) -> Bool {
        guard !isEmpty else { return false }
        return px >= x && px <= x + width && py >= y && py <= y + height
    }

    /// Calcola il rettangolo `aspect-fit` centrato nella vista. Se le dimensioni del video non sono ancora note,
    /// occupa l'intera vista.
    static func aspectFit(viewWidth: Double, viewHeight: Double, videoWidth: Int, videoHeight: Int) -> VideoRect {
        guard viewWidth > 0, viewHeight > 0 else {
            return VideoRect(x: 0, y: 0, width: 0, height: 0)
        }
        guard videoWidth > 0, videoHeight > 0 else {
            return VideoRect(x: 0, y: 0, width: viewWidth, height: viewHeight)
        }
        let vw = Double(videoWidth)
        let vh = Double(videoHeight)
        let scale = min(viewWidth / vw, viewHeight / vh)
        let w = vw * scale
        let h = vh * scale
        return VideoRect(x: (viewWidth - w) * 0.5, y: (viewHeight - h) * 0.5, width: w, height: h)
    }

    /// Normalizza un punto della vista nel formato `[0, 65535]` del protocollo Phantom, fissandolo ai bordi del video.
    func normalize(x px: Double, y py: Double) -> (x: UInt16, y: UInt16) {
        guard !isEmpty else { return (0, 0) }
        let nx = min(1.0, max(0.0, (px - x) / width))
        let ny = min(1.0, max(0.0, (py - y) / height))
        return (UInt16((nx * 65535.0).rounded()), UInt16((ny * 65535.0).rounded()))
    }
}

/// Dito normalizzato indipendente dai tipi generati da UniFFI (mappato poi a `TouchPoint`).
struct NormalizedFinger: Equatable {
    var id: UInt8
    var x: UInt16
    var y: UInt16
    var pressure: UInt8
}

/// Assegna identificatori stabili `0..<maxFingers` alle dita attive sullo schermo e calcola lo stato completo del tocco.
final class TouchTracker {
    static let defaultMaxFingers = 10

    private struct ActiveTouch {
        var slot: UInt8
        var x: UInt16
        var y: UInt16
        var pressure: UInt8
    }

    private let maxFingers: Int
    private var active: [UInt: ActiveTouch] = [:]
    private var usedSlots: Set<UInt8> = []

    init(maxFingers: Int = TouchTracker.defaultMaxFingers) {
        self.maxFingers = max(1, min(10, maxFingers))
    }

    /// Stato corrente delle dita appoggiate, ordinato per `id` stabile.
    var currentFingers: [NormalizedFinger] {
        active.values
            .sorted { $0.slot < $1.slot }
            .map { NormalizedFinger(id: $0.slot, x: $0.x, y: $0.y, pressure: $0.pressure) }
    }

    /// Registra un nuovo contatto. Se cade nelle bande nere esterne (`requireInsideRect`) o supera `maxFingers`,
    /// viene ignorato.
    @discardableResult
    func begin(touchID: UInt, x: Double, y: Double, pressure: UInt8 = 128, in rect: VideoRect, requireInsideRect: Bool = true) -> Bool {
        if let existing = active[touchID] {
            let norm = rect.normalize(x: x, y: y)
            active[touchID] = ActiveTouch(slot: existing.slot, x: norm.x, y: norm.y, pressure: pressure)
            return true
        }
        guard !rect.isEmpty else { return false }
        if requireInsideRect && !rect.contains(x: x, y: y) { return false }
        guard let slot = allocateSlot() else { return false }
        let norm = rect.normalize(x: x, y: y)
        active[touchID] = ActiveTouch(slot: slot, x: norm.x, y: norm.y, pressure: pressure)
        return true
    }

    /// Aggiorna un contatto già attivo (i movimenti trascinati oltre il bordo restano ancorati al bordo del video).
    func move(touchID: UInt, x: Double, y: Double, pressure: UInt8 = 128, in rect: VideoRect) {
        guard let existing = active[touchID], !rect.isEmpty else { return }
        let norm = rect.normalize(x: x, y: y)
        active[touchID] = ActiveTouch(slot: existing.slot, x: norm.x, y: norm.y, pressure: pressure)
    }

    /// Rimuove un contatto terminato o annullato, liberandone lo slot.
    func end(touchID: UInt) {
        guard let removed = active.removeValue(forKey: touchID) else { return }
        usedSlots.remove(removed.slot)
    }

    func reset() {
        active.removeAll()
        usedSlots.removeAll()
    }

    private func allocateSlot() -> UInt8? {
        for candidate in 0..<UInt8(maxFingers) where !usedSlots.contains(candidate) {
            usedSlots.insert(candidate)
            return candidate
        }
        return nil
    }
}

/// Parsing di indirizzi `ip` / `ip:porta` e certificati DER fissati per QUIC.
enum EndpointParser {
    static let defaultQuicPort: UInt16 = 47800

    /// Normalizza un indirizzo inserito dall'utente in `ip:porta`. Restituisce `nil` se vuoto o malformato.
    static func normalizeAddress(_ raw: String, defaultPort: UInt16 = defaultQuicPort) -> String? {
        let s = raw.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !s.isEmpty else { return nil }

        // Formato `[ipv6]:porta`
        if s.hasPrefix("[") {
            guard let close = s.firstIndex(of: "]") else { return nil }
            let host = String(s[s.index(after: s.startIndex)..<close])
            guard !host.isEmpty else { return nil }
            let rest = s[s.index(after: close)...]
            if rest.isEmpty {
                return "[\(host)]:\(defaultPort)"
            }
            guard rest.hasPrefix(":"), let port = parsePort(String(rest.dropFirst())) else { return nil }
            return "[\(host)]:\(port)"
        }

        let colons = s.filter { $0 == ":" }.count
        if colons == 0 {
            // IPv4 (o nome privo di spazi) senza porta
            guard !s.contains(where: { $0.isWhitespace }) else { return nil }
            return "\(s):\(defaultPort)"
        } else if colons == 1 {
            // `ipv4:porta`
            let parts = s.split(separator: ":", omittingEmptySubsequences: false)
            guard parts.count == 2, !parts[0].isEmpty, let port = parsePort(String(parts[1])) else { return nil }
            return "\(parts[0]):\(port)"
        } else {
            // IPv6 nudo senza parentesi
            guard !s.contains(where: { $0.isWhitespace }) else { return nil }
            return "[\(s)]:\(defaultPort)"
        }
    }

    private static func parsePort(_ raw: String) -> UInt16? {
        guard !raw.isEmpty, let p = UInt16(raw), p > 0 else { return nil }
        return p
    }
}

/// Codifica/decodifica del certificato DER dell'host (esadecimale o Base64) per il pinning QUIC.
enum CertificateCodec {
    /// Decodifica una stringa esadecimale (con eventuali spazi/`:`) o Base64 nei byte DER del certificato.
    static func decode(_ raw: String) -> Data? {
        let trimmed = raw.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { return nil }

        let cleanedHex = trimmed.filter { !$0.isWhitespace && $0 != ":" && $0 != "-" }
        if !cleanedHex.isEmpty, cleanedHex.count % 2 == 0, cleanedHex.allSatisfy({ $0.isHexDigit }) {
            var bytes = [UInt8]()
            bytes.reserveCapacity(cleanedHex.count / 2)
            var idx = cleanedHex.startIndex
            while idx < cleanedHex.endIndex {
                let next = cleanedHex.index(idx, offsetBy: 2)
                guard let b = UInt8(cleanedHex[idx..<next], radix: 16) else { return nil }
                bytes.append(b)
                idx = next
            }
            return Data(bytes)
        }

        let compactBase64 = trimmed
            .components(separatedBy: .whitespacesAndNewlines)
            .joined()
        return Data(base64Encoded: compactBase64)
    }

    /// Rappresentazione esadecimale compatta (minuscola), utile per copia/incolla e diagnostica.
    static func encodeHex(_ data: Data) -> String {
        data.map { String(format: "%02x", $0) }.joined()
    }
}
