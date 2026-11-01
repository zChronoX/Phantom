import Foundation

// Test del riassunto statistico del banco di prova. Senza XCTest: si compila con swiftc (vedi run-tests.sh).

var failures = 0
func check(_ cond: Bool, _ msg: String) {
    print((cond ? "ok   " : "FAIL ") + msg)
    if !cond { failures += 1 }
}
func near(_ a: Double, _ b: Double, _ eps: Double = 1e-9) -> Bool { abs(a - b) < eps }

check(near(BenchSummary.percentile([1, 2, 3, 4, 5], 0.5), 3), "mediana di 1…5 = 3")
check(near(BenchSummary.percentile([1, 2, 3, 4], 0.5), 2.5), "mediana di 1…4 = 2,5 (interpolata)")
check(near(BenchSummary.percentile([10], 0.95), 10), "un solo valore")
check(BenchSummary.percentile([], 0.5).isNaN, "serie vuota = NaN")
check(near(BenchSummary.percentile([1, 2, 3], 0), 1) && near(BenchSummary.percentile([1, 2, 3], 1), 3), "estremi")
check(near(BenchSummary.percentile([1, 2, 3], 2), 3) && near(BenchSummary.percentile([1, 2, 3], -1), 1), "q fuori da 0…1 viene limitato")

let hundred = (1...100).map { Double($0) }
let s = BenchSummary(hundred.shuffled())
check(s.count == 100, "conteggio")
check(near(s.p50, 50.5), "p50 di 1…100 = 50,5")
check(near(s.p95, 95.05, 1e-6), "p95 di 1…100 = 95,05")
check(near(s.p99, 99.01, 1e-6), "p99 di 1…100 = 99,01")
check(near(s.max, 100) && near(s.mean, 50.5), "massimo e media")
check(s.text == "p50 50.5 · p95 95.0 · max 100.0 ms" || s.text == "p50 50.5 · p95 95.1 · max 100.0 ms", "testo leggibile (\(s.text))")

let withNaN = BenchSummary([1, .nan, 3, .nan])
check(withNaN.count == 2 && near(withNaN.p50, 2), "i valori NaN (misure mancanti) vengono ignorati")
check(BenchSummary([]).text == "–" && BenchSummary().count == 0, "serie vuota: trattino")

print(failures == 0 ? "\nTutti i test superati" : "\n\(failures) test falliti")
exit(failures == 0 ? 0 : 1)
