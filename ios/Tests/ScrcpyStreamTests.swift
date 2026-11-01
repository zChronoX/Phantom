import Foundation

// Test del parser scrcpy e delle utilità Annex-B. Senza XCTest: si compila con swiftc ed
// esce con codice diverso da zero se qualcosa non torna (vedi run-tests.sh).

var failures = 0
func check(_ cond: Bool, _ msg: String) {
    print((cond ? "ok   " : "FAIL ") + msg)
    if !cond { failures += 1 }
}

func be32(_ v: UInt32) -> [UInt8] { [UInt8(v >> 24), UInt8((v >> 16) & 0xFF), UInt8((v >> 8) & 0xFF), UInt8(v & 0xFF)] }
func be64(_ v: UInt64) -> [UInt8] { be32(UInt32(v >> 32)) + be32(UInt32(v & 0xFFFF_FFFF)) }

let config: [UInt8] = [0, 0, 0, 1, 0x67, 0x42, 0xC0, 0x1F, 0, 0, 0, 1, 0x68, 0xCE, 0x3C, 0x80]

func makeStream(frames: Int, dummy: Bool = true) -> [UInt8] {
    var s: [UInt8] = dummy ? [0] : []
    s += Array("POCO F4 GT".utf8) + [UInt8](repeating: 0, count: 54)
    s += be32(0x6832_3634)
    s += be32(0x8000_0000) + be32(1080) + be32(2400)
    s += be64(1 << 62) + be32(UInt32(config.count)) + config
    for i in 0..<frames {
        let body: [UInt8] = [0, 0, 0, 1, 0x65] + [UInt8](repeating: UInt8(i), count: 20)
        let key: UInt64 = i % 10 == 0 ? 1 << 61 : 0
        s += be64(key | UInt64(i * 8333)) + be32(UInt32(body.count)) + body
    }
    return s
}

func parse(_ bytes: [UInt8], chunk: Int, dummy: Bool = true) -> [ScrcpyEvent] {
    var p = ScrcpyStreamParser(expectDummyByte: dummy)
    var events: [ScrcpyEvent] = []
    var i = 0
    while i < bytes.count {
        let end = min(i + chunk, bytes.count)
        events += p.feed(Data(bytes[i..<end]))
        i = end
    }
    return events
}

// 1. Lo stesso flusso, dato in pezzi di dimensioni diverse, produce gli stessi eventi.
let stream = makeStream(frames: 25)
let reference = parse(stream, chunk: stream.count)
check(reference.count == 4 + 25, "eventi: nome, codec, sessione, config e 25 frame (\(reference.count))")
for chunk in [1, 2, 7, 13, 64, 1000] {
    check(parse(stream, chunk: chunk) == reference, "pezzi da \(chunk) byte: stesso risultato")
}

// 2. Contenuto degli eventi.
check(reference[0] == .deviceName("POCO F4 GT"), "nome del dispositivo")
check(reference[1] == .codec(ScrcpyCodec.h264.rawValue), "codec h264")
check(reference[2] == .session(width: 1080, height: 2400), "sessione 1080x2400")
check(reference[3] == .config(Data(config)), "pacchetto di configurazione")
if case let .frame(data, pts, key) = reference[4] {
    check(key && pts == 0 && data.count == 25 && data[data.startIndex + 4] == 0x65, "primo frame: keyframe, PTS 0")
} else { check(false, "il quinto evento è un frame") }
if case let .frame(_, pts, key) = reference[5] {
    check(!key && pts == 8333, "secondo frame: non keyframe, PTS 8333 µs")
} else { check(false, "il sesto evento è un frame") }
var keyFrames = 0
for case let .frame(_, _, key) in reference where key { keyFrames += 1 }
check(keyFrames == 3, "3 keyframe su 25 frame (\(keyFrames))")

// 3. Senza byte fittizio (modalità reverse).
check(parse(makeStream(frames: 3, dummy: false), chunk: 5, dummy: false).count == 4 + 3, "senza byte fittizio")

// 4. Nuovo pacchetto di sessione dopo una rotazione.
var rotated = makeStream(frames: 1)
rotated += be32(0x8000_0000) + be32(2400) + be32(1080)
let rotatedEvents = parse(rotated, chunk: 9)
check(rotatedEvents.last == .session(width: 2400, height: 1080), "pacchetto di sessione dopo la rotazione")

// 5. Flussi non validi.
var bad = [UInt8](repeating: 0, count: 1 + 64) + be32(0) // codec id 0 = video disattivato
check(parse(bad, chunk: 100).contains(.failure("il server ha disattivato il video")), "codec id 0")
bad = [UInt8](repeating: 0, count: 1 + 64) + be32(0x6832_3634) + be64(0) + be32(0x7FFF_FFFF)
check(parse(bad, chunk: 100).contains { if case .failure = $0 { return true } else { return false } }, "pacchetto da 2 GB rifiutato")
var parser = ScrcpyStreamParser()
_ = parser.feed(Data(bad))
check(parser.isFailed && parser.feed(Data([1, 2, 3])).isEmpty, "dopo un errore il parser resta fermo")

// 6. Annex-B: divisione in NAL con start code da 3 e da 4 byte.
let mixed = Data([0, 0, 0, 1, 0x67, 1, 2, 0, 0, 1, 0x68, 3, 0, 0, 0, 1, 0x65, 4, 5, 6])
let nals = AnnexB.splitNALs(mixed)
check(nals == [Data([0x67, 1, 2]), Data([0x68, 3]), Data([0x65, 4, 5, 6])], "splitNALs con start code misti")
check(AnnexB.splitNALs(Data([1, 2, 3])).isEmpty, "nessuno start code, nessuna NAL")
check(AnnexB.splitNALs(Data()).isEmpty, "dati vuoti")

// 7. Tipi di NAL e parameter set.
check(AnnexB.nalType(Data([0x67]), codec: .h264) == 7 && AnnexB.nalType(Data([0x65]), codec: .h264) == 5, "tipi H.264")
check(AnnexB.nalType(Data([0x40, 1]), codec: .h265) == 32 && AnnexB.nalType(Data([0x42, 1]), codec: .h265) == 33
      && AnnexB.nalType(Data([0x44, 1]), codec: .h265) == 34 && AnnexB.nalType(Data([0x26, 1]), codec: .h265) == 19, "tipi H.265")
let h264ps = AnnexB.parameterSets(from: AnnexB.splitNALs(Data(config)), codec: .h264)
check(h264ps.sps == Data([0x67, 0x42, 0xC0, 0x1F]) && h264ps.pps == Data([0x68, 0xCE, 0x3C, 0x80]) && h264ps.vps == nil, "SPS e PPS H.264")
let h265cfg = Data([0, 0, 0, 1, 0x40, 1, 0x0C, 0, 0, 0, 1, 0x42, 1, 0x01, 0, 0, 0, 1, 0x44, 1, 0xC1])
let h265ps = AnnexB.parameterSets(from: AnnexB.splitNALs(h265cfg), codec: .h265)
check(h265ps.vps != nil && h265ps.sps != nil && h265ps.pps != nil, "VPS, SPS e PPS H.265")
check(AnnexB.isParameterSet(Data([0x42, 1]), codec: .h265) && !AnnexB.isParameterSet(Data([0x26, 1]), codec: .h265)
      && AnnexB.isParameterSet(Data([0x68]), codec: .h264) && !AnnexB.isParameterSet(Data([0x65]), codec: .h264), "riconoscimento dei parameter set")

// 8. Prefisso di lunghezza.
let avcc = AnnexB.toLengthPrefixed([Data([0x65, 9, 9]), Data([0x41])])
check(avcc == Data([0, 0, 0, 3, 0x65, 9, 9, 0, 0, 0, 1, 0x41]), "formato con prefisso di lunghezza a 4 byte")

print(failures == 0 ? "\nTutti i test superati" : "\n\(failures) test falliti")
exit(failures == 0 ? 0 : 1)
