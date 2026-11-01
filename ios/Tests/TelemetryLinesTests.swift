import Foundation

// Test della parte pura della scatola nera sull'iPhone: spezzare i byte in righe e leggere un campione T.
// Senza XCTest: si compila con swiftc ed esce con codice diverso da zero se qualcosa non torna (vedi run-tests.sh).

var failures = 0
func check(_ cond: Bool, _ msg: String) {
    print((cond ? "ok   " : "FAIL ") + msg)
    if !cond { failures += 1 }
}

// 1. Righe spezzate in pezzi qualsiasi.
var buf = TelemetryLineBuffer()
check(buf.append(Data("H telemetria 1 son".utf8)).isEmpty, "una riga incompleta resta in attesa")
let l1 = buf.append(Data("da=0.5.0\nT 1.0 e=1 | c u1\nS 2.0 ini".utf8))
check(l1 == ["H telemetria 1 sonda=0.5.0", "T 1.0 e=1 | c u1"], "le righe complete escono, anche se arrivano a pezzi")
let l2 = buf.append(Data("zio: 5 task\r\n\n\nB 3.0\n".utf8))
check(l2 == ["S 2.0 inizio: 5 task", "B 3.0"], "la riga rimasta in attesa si completa; CR e righe vuote vengono ignorati")

// 2. Un byte alla volta.
var one = TelemetryLineBuffer()
var got: [String] = []
for b in Array("T 1.0 x\nT 2.0 y\n".utf8) { got += one.append(Data([b])) }
check(got == ["T 1.0 x", "T 2.0 y"], "funziona anche un byte alla volta")

// 3. Flusso non valido: nessun a capo per troppo tempo.
var bad = TelemetryLineBuffer()
_ = bad.append(Data(repeating: 0x41, count: 9000))
check(bad.discarded == 1, "una riga enorme senza a capo viene scartata")
check(bad.append(Data("T 1.0 ok\n".utf8)) == ["T 1.0 ok"], "dopo lo scarto il flusso riprende")

// 4. UTF-8 spezzato a metà di un carattere.
var u = TelemetryLineBuffer()
let accent = Array("è\n".utf8)
check(u.append(Data([accent[0]])).isEmpty && u.append(Data(accent[1...])) == ["è"], "un carattere UTF-8 diviso tra due pezzi si ricompone")

// 5. Campione T completo, nel formato scritto dalla sonda.
let t = "T 1787.907 e=1793553100 | c u12 s8 w3 q2 | l 0.50 0.40 2/300 b3 | m a4882 f781 d100 w0 s1953 as0 pd0 | p c0.0 m0.0/0.0 i12.5/3.1 | g b35 c818 k200 | t cpu-0-0=55.6 gpuss-0=50.1 | f 1804/2419/2995 | P sf S/do_epoll_wait t25 D0 ss D/io_schedule t210 D2[Binder:f2fs_sync] | R 4504 giro48ms in1.0M out0.9M fr10/9 dr0 q0K | a core5 hw40 task300"
if let s = TelemetrySummary.parse(t) {
    check(s.epoch == 1793553100, "ora (epoch) del campione")
    check(s.cpu == "u12 s8 w3 q2", "CPU")
    check(s.blocked == 3, "task bloccati nel kernel")
    check(s.load == "0.50 0.40 2/300 b3", "carico")
    check(s.memory.hasPrefix("a4882 f781"), "memoria")
    check(s.pressure == "c0.0 m0.0/0.0 i12.5/3.1", "pressione")
    check(s.gpu == "b35 c818 k200", "GPU")
    check(s.temps == "cpu-0-0=55.6 gpuss-0=50.1", "temperature")
    check(s.freq == "1804/2419/2995", "frequenze")
    check(s.procs.contains("ss D/io_schedule"), "processi chiave con stato nel kernel")
    check(s.relay.hasPrefix("4504 giro48ms"), "ponte video")
    check(s.ages == "core5 hw40 task300", "età dei campionatori")
    check(s.stuck.isEmpty, "nessun campionatore incastrato")
    check(s.rows.count == 10, "dieci righe da mostrare (\(s.rows.count))")
} else {
    check(false, "il campione T viene letto")
}

// 6. Campionatore incastrato: la parte è sostituita da FERMO(...).
let st = "T 9.9 e=1 | c u1 s1 w0 q0 | l 0.1 0.1 1/9 b0 | m a1 f1 d0 w0 s1 as0 pd0 | p c0.0 m0.0/0.0 i0.0/0.0 | FERMO(gpu:gpuclk 5230ms) | P sf S/x t1 D0 | R - | a core5 hw5230 task300"
if let s = TelemetrySummary.parse(st) {
    check(s.stuck == ["FERMO(gpu:gpuclk 5230ms)"], "il campionatore incastrato viene riportato")
    check(s.rows.contains(where: { $0.0 == "INCASTRATO" }), "e compare tra le righe, evidenziato")
    check(s.gpu.isEmpty, "senza i dati della GPU")
} else {
    check(false, "il campione con FERMO viene letto")
}

// 7. Robustezza.
check(TelemetrySummary.parse("S 1.0 inizio: x") == nil, "una riga S non è un campione")
check(TelemetrySummary.parse("T 1.0") != nil, "un campione quasi vuoto non rompe nulla")
check(TelemetrySummary.parse("T 1.0 e=abc | | - | zzz") != nil, "segmenti vuoti o sconosciuti vengono ignorati")
check(TelemetrySummary.parse("T 1.0 e=77")?.epoch == 77, "epoch letto anche da solo")

// 8. Tipi di riga.
check(TelemetryKind(line: "H x") == .header && TelemetryKind(line: "T x") == .sample && TelemetryKind(line: "S x") == .stall
      && TelemetryKind(line: "D x") == .blockedTask && TelemetryKind(line: "K x") == .kernel && TelemetryKind(line: "B x") == .beat
      && TelemetryKind(line: "? x") == .other, "classificazione delle righe per prima lettera")

print(failures == 0 ? "\nTutti i test superati" : "\n\(failures) test falliti")
exit(failures == 0 ? 0 : 1)
