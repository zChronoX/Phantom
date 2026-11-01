import Foundation
import Network
import UIKit

/// Statistiche di un test, calcolate sulla coda di rete e pubblicate sul main thread.
struct PingStats {
    var sent = 0
    var received = 0
    var p50 = Double.nan
    var p95 = Double.nan
    var p99 = Double.nan
    var max = Double.nan
    var jitter = Double.nan
    var spikes = 0
    var phoneSleptMs: UInt64 = 0

    var lossPercent: Double { sent > 0 ? Double(sent - received) / Double(sent) * 100 : 0 }
}

/// Test di latenza UDP verso phantom-probe (porta 47800).
/// Tutto lo stato interno vive su `queue`; l'interfaccia legge solo le proprietà @Published.
final class PingTester: ObservableObject {
    @Published private(set) var running = false
    @Published private(set) var stats = PingStats()
    @Published private(set) var recent: [Double] = []
    @Published private(set) var status = "Pronto"
    @Published private(set) var report = ""

    private let queue = DispatchQueue(label: "app.phantom.f0.ping", qos: .userInteractive)
    private var connection: NWConnection?
    private var timer: DispatchSourceTimer?

    // Stato del test: accesso solo da `queue`.
    private var seq: UInt32 = 0
    private var total = 0
    private var rttBySeq: [UInt32: Double] = [:]
    private var firstSuspend: UInt64?
    private var lastSuspend: UInt64?
    private var config = Config(host: "", seconds: 30, intervalMs: 10, size: 64, voice: true, device: "")

    struct Config {
        var host: String
        var seconds: Double
        var intervalMs: Double
        var size: Int
        var voice: Bool
        var device = ""
    }

    // MARK: - Comandi dall'interfaccia (main thread)

    func start(_ cfg: Config) {
        guard !running else { return }
        let host = cfg.host.trimmingCharacters(in: .whitespaces)
        guard !host.isEmpty, let port = NWEndpoint.Port(rawValue: 47800) else {
            status = "Inserisci l'indirizzo IP del POCO"
            return
        }
        var cfg = cfg
        cfg.device = "\(UIDevice.current.model) iOS \(UIDevice.current.systemVersion)"
        running = true
        report = ""
        stats = PingStats()
        recent = []
        status = "Connessione a \(host)…"
        UIApplication.shared.isIdleTimerDisabled = true

        let params = NWParameters.udp
        // .interactiveVoice = DSCP EF / coda "voce" del Wi-Fi (WMM): è ciò che userà Phantom.
        params.serviceClass = cfg.voice ? .interactiveVoice : .bestEffort
        let conn = NWConnection(host: NWEndpoint.Host(host), port: port, using: params)

        queue.async {
            self.config = cfg
            self.config.host = host
            self.seq = 0
            self.total = Int(cfg.seconds * 1000 / cfg.intervalMs)
            self.rttBySeq = [:]
            self.firstSuspend = nil
            self.lastSuspend = nil
            self.connection = conn
        }

        conn.stateUpdateHandler = { [weak self] state in
            guard let self = self else { return }
            switch state {
            case .ready:
                self.publishStatus("Test in corso…")
                self.beginSending()
            case .waiting(let error):
                self.publishStatus("In attesa della rete: \(error.localizedDescription). Controlla il permesso \"Rete locale\" in Impostazioni.")
            case .failed(let error):
                self.publishStatus("Connessione fallita: \(error.localizedDescription)")
                self.finish()
            default:
                break
            }
        }
        conn.start(queue: queue)
        receive(on: conn)
    }

    func stop() {
        queue.async { self.finish() }
    }

    // MARK: - Invio e ricezione (queue)

    private func beginSending() {
        let t = DispatchSource.makeTimerSource(flags: .strict, queue: queue)
        t.schedule(deadline: .now(), repeating: .nanoseconds(Int(config.intervalMs * 1_000_000)), leeway: .nanoseconds(0))
        t.setEventHandler { [weak self] in self?.tick() }
        timer = t
        t.resume()
    }

    private func tick() {
        guard let conn = connection else { return }
        if Int(seq) >= total {
            timer?.cancel()
            timer = nil
            // Un secondo per far arrivare le ultime risposte, poi il riepilogo.
            queue.asyncAfter(deadline: .now() + 1) { [weak self] in self?.finish() }
            return
        }
        let packet = ProbePacket.make(seq: seq, clientNs: DispatchTime.now().uptimeNanoseconds, size: config.size)
        conn.send(content: packet, completion: .idempotent)
        seq &+= 1
        if seq % 25 == 0 { publishStats() }
    }

    private func receive(on conn: NWConnection) {
        conn.receiveMessage { [weak self] data, _, _, error in
            guard let self = self else { return }
            if let data = data, let reply = ProbePacket.parse(data) {
                let rtt = Double(DispatchTime.now().uptimeNanoseconds &- reply.clientNs) / 1_000_000
                if reply.seq < self.seq, self.rttBySeq[reply.seq] == nil {
                    self.rttBySeq[reply.seq] = rtt
                    if self.firstSuspend == nil { self.firstSuspend = reply.serverSuspendMs }
                    self.lastSuspend = reply.serverSuspendMs
                }
            }
            if error == nil, self.connection === conn {
                self.receive(on: conn)
            }
        }
    }

    private func finish() {
        guard connection != nil else { return }
        timer?.cancel()
        timer = nil
        connection?.cancel()
        connection = nil
        let s = computeStats()
        let text = makeReport(s)
        DispatchQueue.main.async {
            self.stats = s
            self.report = text
            self.running = false
            self.status = s.received > 0 ? "Test completato" : "Nessuna risposta dalla sonda"
            UIApplication.shared.isIdleTimerDisabled = false
        }
    }

    // MARK: - Statistiche (queue)

    private func computeStats() -> PingStats {
        var s = PingStats()
        s.sent = Int(seq)
        let ordered = rttBySeq.sorted { $0.key < $1.key }.map { $0.value }
        s.received = ordered.count
        guard !ordered.isEmpty else { return s }
        let sorted = ordered.sorted()
        func pct(_ p: Double) -> Double { sorted[min(sorted.count - 1, Int(p / 100 * Double(sorted.count)))] }
        s.p50 = pct(50)
        s.p95 = pct(95)
        s.p99 = pct(99)
        s.max = sorted[sorted.count - 1]
        if ordered.count > 1 {
            var sum = 0.0
            for i in 1..<ordered.count { sum += abs(ordered[i] - ordered[i - 1]) }
            s.jitter = sum / Double(ordered.count - 1)
        }
        s.spikes = ordered.filter { $0 > 50 }.count
        if let a = firstSuspend, let b = lastSuspend, b >= a { s.phoneSleptMs = b - a }
        return s
    }

    private func publishStats() {
        let s = computeStats()
        let last = rttBySeq.sorted { $0.key < $1.key }.suffix(300).map { $0.value }
        DispatchQueue.main.async {
            self.stats = s
            self.recent = last
        }
    }

    private func publishStatus(_ text: String) {
        DispatchQueue.main.async { self.status = text }
    }

    private func makeReport(_ s: PingStats) -> String {
        guard s.received > 0 else { return "" }
        let date = ISO8601DateFormatter().string(from: Date())
        return """
        Phantom F0 · test di rete iPhone → Android (UDP nativo)
        \(date) · \(config.device) → \(config.host)
        classe di servizio \(config.voice ? "voce (WMM)" : "normale") · pacchetti da \(config.size) byte ogni \(fmt(config.intervalMs, 0)) ms
        ricevuti \(s.received)/\(s.sent) · persi \(fmt(s.lossPercent, 2))%
        RTT p50 \(fmt(s.p50)) · p95 \(fmt(s.p95)) · p99 \(fmt(s.p99)) · max \(fmt(s.max)) ms
        jitter \(fmt(s.jitter, 2)) ms · picchi >50 ms: \(s.spikes) · il telefono ha dormito \(s.phoneSleptMs) ms
        """
    }

    private func fmt(_ v: Double, _ digits: Int = 1) -> String {
        v.isNaN ? "–" : String(format: "%.\(digits)f", v)
    }
}
