import Foundation

// Parte «pura» della scatola nera sull'iPhone: nessun UIKit né rete, così si prova con swiftc (ios/Tests).

/// Spezza i byte ricevuti in righe di testo (separate da \n). L'ultima riga incompleta resta in attesa
/// del prossimo pezzo. Una riga che supera `maxLine` byte senza andare a capo è un flusso non valido: si scarta.
struct TelemetryLineBuffer {
    private var pending = Data()
    private let maxLine = 8192
    private(set) var discarded = 0

    mutating func append(_ data: Data) -> [String] {
        pending.append(data)
        var lines: [String] = []
        while let nl = pending.firstIndex(of: 0x0A) {
            let lineData = pending[pending.startIndex..<nl]
            var text = String(decoding: lineData, as: UTF8.self)
            pending.removeSubrange(pending.startIndex...nl)
            if text.hasSuffix("\r") { text.removeLast() }
            if !text.isEmpty { lines.append(text) }
        }
        if pending.count > maxLine {
            pending.removeAll()
            discarded += 1
        }
        return lines
    }
}

/// Le parti di un campione «T» della scatola nera, già divise per l'interfaccia.
/// Formato: `T <secondi> e=<epoch> | c … | l … b<N> | m … | p … | g … | t … | f … | P … | R … | a …`
struct TelemetrySummary: Equatable {
    var epoch: Int?
    var cpu = ""          // c: percentuali utente/sistema/attesa disco/interrupt
    var load = ""         // l: carico, processi in coda, task bloccati
    var memory = ""       // m: memoria disponibile/libera (MB), sporca, scritture, swap, stalli di allocazione
    var pressure = ""     // p: pressione PSI (% di tempo fermo): CPU, memoria, I/O
    var gpu = ""          // g: occupazione, frequenza, memoria allocata dal driver
    var temps = ""        // t: temperature
    var freq = ""         // f: frequenze CPU
    var procs = ""        // P: stato dei processi chiave (surfaceflinger, system_server, server di cattura…)
    var relay = ""        // R: ponte video
    var ages = ""         // a: età dei campionatori in ms
    var stuck: [String] = []   // campionatori incastrati, es. «FERMO(gpu:gpuclk 5230ms)»
    var blocked: Int?     // task bloccati nel kernel (stato D)

    static func parse(_ line: String) -> TelemetrySummary? {
        guard line.hasPrefix("T ") else { return nil }
        var s = TelemetrySummary()
        for raw in line.components(separatedBy: " | ") {
            let seg = raw.trimmingCharacters(in: .whitespaces)
            guard let first = seg.first else { continue }
            let rest = String(seg.dropFirst()).trimmingCharacters(in: .whitespaces)
            switch first {
            case "T":
                if let r = seg.range(of: "e=") {
                    let digits = seg[r.upperBound...].prefix { $0.isNumber }
                    s.epoch = Int(digits)
                }
            case "c": s.cpu = rest
            case "l":
                s.load = rest
                if let r = rest.range(of: " b", options: .backwards) { s.blocked = Int(rest[r.upperBound...]) }
            case "m": s.memory = rest
            case "p": s.pressure = rest
            case "g": s.gpu = rest
            case "t": s.temps = rest
            case "f": s.freq = rest
            case "P": s.procs = rest
            case "R": s.relay = rest
            case "a": s.ages = rest
            case "F": s.stuck.append(seg)
            default: break
            }
        }
        return s
    }

    /// Righe «etichetta: valore» per mostrarlo a video (solo quelle non vuote).
    var rows: [(String, String)] {
        var r: [(String, String)] = []
        func add(_ k: String, _ v: String) { if !v.isEmpty && v != "-" { r.append((k, v)) } }
        add("CPU % (utente sistema disco irq)", cpu)
        add("Carico · coda · task bloccati", load)
        add("Memoria MB (disp. libera …)", memory)
        add("Pressione % (CPU · mem · I/O)", pressure)
        add("GPU (uso % · MHz · MB driver)", gpu)
        add("Temperature °C", temps)
        add("Frequenze CPU MHz", freq)
        add("Processi", procs)
        add("Ponte video", relay)
        add("Età campionatori ms", ages)
        for st in stuck { add("INCASTRATO", st) }
        return r
    }
}

/// Classifica una riga del flusso per la prima lettera.
enum TelemetryKind {
    case header, sample, stall, blockedTask, kernel, beat, other

    init(line: String) {
        guard let c = line.first else { self = .other; return }
        switch c {
        case "H": self = .header
        case "T": self = .sample
        case "S": self = .stall
        case "D": self = .blockedTask
        case "K": self = .kernel
        case "B": self = .beat
        default: self = .other
        }
    }
}
