import Foundation

// Test delle conversioni AVCC ↔ Annex-B. Si compila con swiftc insieme a PhantomF0/AnnexBConvert.swift e
// PhantomF0/ScrcpyStream.swift (usato per ricontrollare il risultato) ed esce con codice 1 se qualcosa non torna.

var failures = 0
func check(_ cond: Bool, _ message: @autoclosure () -> String) {
    if !cond {
        failures += 1
        print("FALLITO: \(message())")
    }
}

func lengthPrefixed(_ nals: [[UInt8]], lengthSize: Int = 4) -> Data {
    var d = Data()
    for n in nals {
        for k in (0..<lengthSize).reversed() { d.append(UInt8((n.count >> (8 * k)) & 0xFF)) }
        d.append(contentsOf: n)
    }
    return d
}

let sc: [UInt8] = [0, 0, 0, 1]
let nal1: [UInt8] = [0x40, 0x01, 0x0C, 0x01]       // tipo 32 (VPS) in HEVC
let nal2: [UInt8] = [0x26, 0x01, 0xAF, 0x08, 0x00, 0x00, 0x03, 0x01] // IDR con byte di emulazione
let nal3: [UInt8] = [0x02, 0x01, 0xD0]

// Andata: due e tre NAL, con prefisso a 4 byte e a 2 byte.
let a = AnnexBConvert.fromLengthPrefixed(lengthPrefixed([nal1, nal2, nal3]))
var expectedA: [UInt8] = []
for n in [nal1, nal2, nal3] { expectedA += sc; expectedA += n }
check([UInt8](a) == expectedA, "conversione a 4 byte: \([UInt8](a))")
let b = AnnexBConvert.fromLengthPrefixed(lengthPrefixed([nal1, nal3], lengthSize: 2), lengthSize: 2)
var expectedB: [UInt8] = []
for n in [nal1, nal3] { expectedB += sc; expectedB += n }
check([UInt8](b) == expectedB, "conversione a 2 byte")

// Ricontrollo con il parser Annex-B dell'app: le NAL tornano identiche.
let nals = AnnexB.splitNALs(a)
check(nals.count == 3, "numero di NAL: \(nals.count)")
if nals.count == 3 {
    check([UInt8](nals[0]) == nal1, "prima NAL")
    check([UInt8](nals[1]) == nal2, "seconda NAL (con byte di emulazione)")
    check([UInt8](nals[2]) == nal3, "terza NAL")
}

// Dati vuoti, troncati o incoerenti: nessun crash, nessuna NAL fuori dal buffer.
check(AnnexBConvert.fromLengthPrefixed(Data()).isEmpty, "vuoto")
check(AnnexBConvert.fromLengthPrefixed(Data([0, 0, 0])).isEmpty, "meno di un prefisso")
check(AnnexBConvert.fromLengthPrefixed(Data([0, 0, 0, 9, 1, 2])).isEmpty, "lunghezza oltre il buffer")
let cut = Data(lengthPrefixed([nal1, nal2]).dropLast(3))
var expectedCut: [UInt8] = sc
expectedCut += nal1
check([UInt8](AnnexBConvert.fromLengthPrefixed(cut)) == expectedCut, "troncato: resta la prima NAL")
check(AnnexBConvert.fromLengthPrefixed(Data([0xFF, 0xFF, 0xFF, 0xFF, 1, 2, 3])).isEmpty, "lunghezza enorme")
check(AnnexBConvert.fromLengthPrefixed(lengthPrefixed([nal1]), lengthSize: 0).isEmpty, "prefisso non valido")
check(AnnexBConvert.fromLengthPrefixed(lengthPrefixed([nal1]), lengthSize: 5).isEmpty, "prefisso troppo lungo")
let withEmpty: [UInt8] = [0, 0, 0, 0, 0, 0, 0, 3, 7, 8, 9]
let expectedEmpty: [UInt8] = [0, 0, 0, 1, 7, 8, 9]
check([UInt8](AnnexBConvert.fromLengthPrefixed(Data(withEmpty))) == expectedEmpty, "NAL di lunghezza zero ignorata")

// Parameter set.
let ps = AnnexBConvert.parameterSets([Data(nal1), Data(), Data(nal3)])
check([UInt8](ps) == expectedB, "parameter set")
check(AnnexBConvert.parameterSets([]).isEmpty, "nessun parameter set")

if failures > 0 {
    print("\(failures) controlli falliti")
    exit(1)
}
print("OK: conversioni AVCC ↔ Annex-B")
