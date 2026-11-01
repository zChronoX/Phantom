import Foundation

/// Riassunto di una serie di misure in millisecondi (parte «pura»: si prova con swiftc in ios/Tests).
struct BenchSummary: Equatable {
    var count = 0
    var p50 = Double.nan
    var p95 = Double.nan
    var p99 = Double.nan
    var max = Double.nan
    var mean = Double.nan

    init() {}

    init(_ values: [Double]) {
        let clean = values.filter { !$0.isNaN }
        count = clean.count
        guard !clean.isEmpty else { return }
        let s = clean.sorted()
        p50 = BenchSummary.percentile(s, 0.50)
        p95 = BenchSummary.percentile(s, 0.95)
        p99 = BenchSummary.percentile(s, 0.99)
        max = s[s.count - 1]
        mean = s.reduce(0, +) / Double(s.count)
    }

    /// Percentile con interpolazione lineare su una serie già ordinata (q tra 0 e 1).
    static func percentile(_ sorted: [Double], _ q: Double) -> Double {
        guard !sorted.isEmpty else { return .nan }
        if sorted.count == 1 { return sorted[0] }
        let qq = Swift.min(Swift.max(q, 0), 1)
        let pos = qq * Double(sorted.count - 1)
        let lo = Int(pos.rounded(.down))
        let hi = Int(pos.rounded(.up))
        return sorted[lo] + (sorted[hi] - sorted[lo]) * (pos - Double(lo))
    }

    /// «p50 3.2 · p95 5.1 · max 9.8 ms»
    var text: String {
        count == 0 ? "–" : String(format: "p50 %.1f · p95 %.1f · max %.1f ms", p50, p95, max)
    }
}
