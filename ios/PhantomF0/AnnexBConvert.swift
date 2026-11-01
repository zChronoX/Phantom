import Foundation

/// Conversioni tra il formato dell'encoder di Apple (NAL con prefisso di lunghezza, AVCC/HVCC) e quello che viaggia nel
/// protocollo di Phantom: Annex-B con start code, come lo produce MediaCodec su Android. Solo Foundation, così si prova
/// con test automatici anche senza iPhone.
enum AnnexBConvert {
    static let startCode: [UInt8] = [0, 0, 0, 1]

    /// Da NAL con prefisso di lunghezza (1–4 byte, big endian) ad Annex-B. Se i dati sono troncati o incoerenti si
    /// ferma all'ultima NAL intera: meglio un frame corto che una NAL che esce dal buffer.
    static func fromLengthPrefixed(_ data: Data, lengthSize: Int = 4) -> Data {
        guard (1...4).contains(lengthSize) else { return Data() }
        let bytes = [UInt8](data)
        var out = Data(capacity: bytes.count + 16)
        var i = 0
        while i + lengthSize <= bytes.count {
            var len = 0
            for k in 0..<lengthSize { len = len << 8 | Int(bytes[i + k]) }
            i += lengthSize
            if len == 0 { continue }
            guard len <= bytes.count - i else { break }
            out.append(contentsOf: startCode)
            out.append(contentsOf: bytes[i..<(i + len)])
            i += len
        }
        return out
    }

    /// Parameter set (VPS/SPS/PPS) ciascuno preceduto da uno start code.
    static func parameterSets(_ sets: [Data]) -> Data {
        var out = Data()
        for s in sets where !s.isEmpty {
            out.append(contentsOf: startCode)
            out.append(s)
        }
        return out
    }
}
