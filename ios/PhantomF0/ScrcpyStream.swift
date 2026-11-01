import Foundation

// Parser del flusso video di scrcpy (protocollo 4.x/5.x, vedi doc/develop.md di scrcpy) e
// utilità Annex-B. Usa solo Foundation, così si prova con test automatici anche senza iPhone.
//
// Sequenza sul socket video (tunnel forward, solo video):
//   1 byte      fittizio
//   64 byte     nome del dispositivo (UTF-8, riempito con zeri)
//   4 byte      id del codec (big endian)
//   12 byte     pacchetto di sessione: flags (bit 31 = 1), larghezza, altezza (u32 big endian)
//   ripetuti:   intestazione da 12 byte + dati
//     - sessione: come sopra (si ripete se il telefono ruota)
//     - media: u64 = [bit63 = 0][bit62 config][bit61 keyframe][PTS in µs, 61 bit], poi u32 = lunghezza

enum ScrcpyCodec: UInt32 {
    case h264 = 0x6832_3634
    case h265 = 0x6832_3635
    case av1 = 0x0061_7631
    case vp8 = 0x0076_7038
    case vp9 = 0x0076_7039
}

enum ScrcpyEvent: Equatable {
    case deviceName(String)
    case codec(UInt32)
    case session(width: Int, height: Int)
    case config(Data)
    case frame(data: Data, ptsUs: UInt64, keyFrame: Bool)
    case failure(String)
}

struct ScrcpyStreamParser {
    static let maxPacketSize = 16 * 1024 * 1024

    private enum State {
        case dummyByte
        case deviceName
        case codecId
        case header
        case payload(size: Int, config: Bool, key: Bool, pts: UInt64)
        case failed
    }

    private var state: State
    private var buf = [UInt8]()
    private var pos = 0

    init(expectDummyByte: Bool = true) {
        state = expectDummyByte ? .dummyByte : .deviceName
    }

    var isFailed: Bool {
        if case .failed = state { return true }
        return false
    }

    /// Aggiunge dati arrivati dalla rete (anche a pezzi di un byte) e restituisce gli eventi completi.
    mutating func feed(_ chunk: Data) -> [ScrcpyEvent] {
        if isFailed { return [] }
        buf.append(contentsOf: chunk)
        var out: [ScrcpyEvent] = []

        loop: while true {
            let available = buf.count - pos
            switch state {
            case .failed:
                break loop

            case .dummyByte:
                guard available >= 1 else { break loop }
                pos += 1
                state = .deviceName

            case .deviceName:
                guard available >= 64 else { break loop }
                let raw = Array(buf[pos..<(pos + 64)])
                pos += 64
                let end = raw.firstIndex(of: 0) ?? raw.count
                out.append(.deviceName(String(decoding: raw[0..<end], as: UTF8.self)))
                state = .codecId

            case .codecId:
                guard available >= 4 else { break loop }
                let id = be32(pos)
                pos += 4
                if id == 0 {
                    fail(&out, "il server ha disattivato il video")
                } else if id == 1 {
                    fail(&out, "errore di configurazione del server scrcpy")
                } else {
                    out.append(.codec(id))
                    state = .header
                }

            case .header:
                guard available >= 12 else { break loop }
                if buf[pos] & 0x80 != 0 {
                    let w = be32(pos + 4), h = be32(pos + 8)
                    pos += 12
                    if w == 0 || h == 0 || w > 16384 || h > 16384 {
                        fail(&out, "dimensioni video non valide: \(w)×\(h)")
                    } else {
                        out.append(.session(width: Int(w), height: Int(h)))
                    }
                } else {
                    let ptsFlags = be64(pos)
                    let size = Int(be32(pos + 8))
                    pos += 12
                    if size > ScrcpyStreamParser.maxPacketSize {
                        fail(&out, "pacchetto troppo grande (\(size) byte): flusso non valido")
                    } else {
                        let config = ptsFlags & (1 << 62) != 0
                        let key = ptsFlags & (1 << 61) != 0
                        state = .payload(size: size, config: config, key: key, pts: ptsFlags & ((1 << 61) - 1))
                    }
                }

            case let .payload(size, config, key, pts):
                guard available >= size else { break loop }
                let data = Data(buf[pos..<(pos + size)])
                pos += size
                state = .header
                if size > 0 {
                    out.append(config ? .config(data) : .frame(data: data, ptsUs: pts, keyFrame: key))
                }
            }
        }

        if pos > 1 << 16 { // ogni tanto si scarta la parte già letta
            buf.removeFirst(pos)
            pos = 0
        }
        return out
    }

    private mutating func fail(_ out: inout [ScrcpyEvent], _ message: String) {
        state = .failed
        out.append(.failure(message))
    }

    private func be32(_ i: Int) -> UInt32 {
        UInt32(buf[i]) << 24 | UInt32(buf[i + 1]) << 16 | UInt32(buf[i + 2]) << 8 | UInt32(buf[i + 3])
    }

    private func be64(_ i: Int) -> UInt64 {
        UInt64(be32(i)) << 32 | UInt64(be32(i + 4))
    }
}

// MARK: - Annex-B

enum AnnexB {
    struct ParameterSets {
        var vps: Data?
        var sps: Data?
        var pps: Data?
    }

    /// Divide un blocco Annex-B (start code 00 00 01 o 00 00 00 01) in NAL, senza start code.
    static func splitNALs(_ data: Data) -> [Data] {
        let bytes = [UInt8](data)
        var starts: [(pos: Int, len: Int)] = []
        var i = 0
        while i + 3 <= bytes.count {
            if bytes[i] == 0 && bytes[i + 1] == 0 {
                if bytes[i + 2] == 1 {
                    starts.append((i, 3)); i += 3; continue
                }
                if i + 4 <= bytes.count && bytes[i + 2] == 0 && bytes[i + 3] == 1 {
                    starts.append((i, 4)); i += 4; continue
                }
            }
            i += 1
        }
        var nals: [Data] = []
        for (k, s) in starts.enumerated() {
            let begin = s.pos + s.len
            let end = k + 1 < starts.count ? starts[k + 1].pos : bytes.count
            if end > begin { nals.append(Data(bytes[begin..<end])) }
        }
        return nals
    }

    static func nalType(_ nal: Data, codec: ScrcpyCodec) -> UInt8 {
        let b = nal[nal.startIndex]
        return codec == .h265 ? (b >> 1) & 0x3F : b & 0x1F
    }

    /// SPS/PPS (H.264) oppure VPS/SPS/PPS (H.265): già descritti dal formato, si tolgono dai frame.
    static func isParameterSet(_ nal: Data, codec: ScrcpyCodec) -> Bool {
        let t = nalType(nal, codec: codec)
        return codec == .h265 ? (32...34).contains(t) : (t == 7 || t == 8)
    }

    static func parameterSets(from nals: [Data], codec: ScrcpyCodec) -> ParameterSets {
        var ps = ParameterSets()
        for nal in nals {
            switch (codec, nalType(nal, codec: codec)) {
            case (.h265, 32): ps.vps = nal
            case (.h265, 33): ps.sps = nal
            case (.h265, 34): ps.pps = nal
            case (.h264, 7): ps.sps = nal
            case (.h264, 8): ps.pps = nal
            default: break
            }
        }
        return ps
    }

    /// NAL con prefisso di lunghezza a 4 byte (formato AVCC/HVCC che VideoToolbox si aspetta).
    static func toLengthPrefixed(_ nals: [Data]) -> Data {
        var out = Data()
        for n in nals {
            var len = UInt32(n.count).bigEndian
            withUnsafeBytes(of: &len) { out.append(contentsOf: $0) }
            out.append(n)
        }
        return out
    }
}
