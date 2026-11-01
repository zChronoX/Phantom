import Foundation
import Network
import Combine

/// Registratore della «scatola nera» del POCO (TCP 47804 sulla sonda).
///
/// Perché sta sull'iPhone: se il POCO si blocca e va riavviato a mano, tutto ciò che era solo sul POCO può andare
/// perso. Qui le righe restano: l'app le scrive su un file (che si condivide dalla schermata) e continua a
/// riprovare la connessione finché il POCO non torna. Il silenzio stesso è un dato: quanti secondi e quando.
///
/// Tutto lo stato interno si tocca SOLO dalla coda `queue`; l'interfaccia riceve copie sul thread principale.
final class TelemetryRecorder: ObservableObject {
    static let shared = TelemetryRecorder()

    @Published private(set) var running = false
    @Published private(set) var connected = false
    @Published private(set) var lineCount = 0
    @Published private(set) var silentSeconds = 0.0
    @Published private(set) var stallCount = 0
    @Published private(set) var summary: TelemetrySummary?
    @Published private(set) var recent: [String] = []
    @Published private(set) var fileURL: URL?
    @Published private(set) var status = "Spenta"

    private let queue = DispatchQueue(label: "app.phantom.f0.telemetry", qos: .utility)
    private var isRunning = false
    private var host = ""
    private var generation = 0
    private var connection: NWConnection?
    private var buffer = TelemetryLineBuffer()
    private var timer: DispatchSourceTimer?

    private var handle: FileHandle?
    private var url: URL?
    private var pendingOut = Data()
    private let stamp: DateFormatter = {
        let f = DateFormatter()
        f.locale = Locale(identifier: "en_US_POSIX")
        f.dateFormat = "HH:mm:ss.SSS"
        return f
    }()

    private var isConnected = false
    private var lastRx = Date()
    private var lastLoggedSilence = Date.distantPast
    private var lines = 0
    private var stalls = 0
    private var latest: TelemetrySummary?
    private var events: [String] = []
    private var ring: [String] = []

    // MARK: comandi (da qualsiasi thread)

    /// Avvia una nuova registrazione (nuovo file). Se ce n'è già una in corso, non fa nulla.
    func start(host: String) {
        queue.async {
            if self.isRunning { return }
            let h = host.trimmingCharacters(in: .whitespaces)
            guard !h.isEmpty else { self.setStatus("Inserisci l'indirizzo IP del POCO"); return }
            self.host = h
            self.isRunning = true
            self.lines = 0; self.stalls = 0; self.latest = nil; self.events.removeAll(); self.ring.removeAll()
            self.openFile()
            self.log("# registrazione avviata verso \(h):47804")
            self.lastRx = Date()
            self.startTimer()
            self.connectLocked()
            self.publish()
        }
    }

    /// Come `start`, ma da usare quando un'altra schermata (il video) vuole la scatola nera accesa.
    func startIfNeeded(host: String) { start(host: host) }

    func stop() {
        queue.async {
            guard self.isRunning else { return }
            self.isRunning = false
            self.generation &+= 1
            self.connection?.stateUpdateHandler = nil
            self.connection?.cancel()
            self.connection = nil
            self.timer?.cancel()
            self.timer = nil
            self.isConnected = false
            self.log("# registrazione fermata")
            self.flushLocked()
            try? self.handle?.close()
            self.handle = nil
            self.setStatusLocked("Fermata")
            self.publish()
        }
    }

    /// Scrive subito su disco quello che è in attesa (da chiamare quando l'app va in background).
    func flush() { queue.async { self.flushLocked() } }

    /// Le ultime `n` righe come testo, per copiarle negli appunti.
    func recentText(_ n: Int = 200) -> String {
        queue.sync { ring.suffix(n).joined(separator: "\n") }
    }

    /// Frase breve per il badge nella schermata video.
    var badge: String {
        if !running { return "Scatola nera spenta" }
        if !connected { return "Scatola nera: non collegata" }
        if silentSeconds > 2 { return String(format: "SCATOLA NERA MUTA DA %.0f s", silentSeconds) }
        return "Scatola nera: collegata · \(lineCount) righe"
    }

    // MARK: file

    private func openFile() {
        let dir = FileManager.default.urls(for: .documentDirectory, in: .userDomainMask)[0]
        // Si tengono gli ultimi 10 registri: il telefono non si riempie.
        if let old = try? FileManager.default.contentsOfDirectory(at: dir, includingPropertiesForKeys: nil)
            .filter({ $0.lastPathComponent.hasPrefix("telemetria-") && $0.pathExtension == "txt" })
            .sorted(by: { $0.lastPathComponent < $1.lastPathComponent }), old.count >= 10 {
            for f in old.prefix(old.count - 9) { try? FileManager.default.removeItem(at: f) }
        }
        let f = DateFormatter()
        f.locale = Locale(identifier: "en_US_POSIX")
        f.dateFormat = "yyyyMMdd-HHmmss"
        let file = dir.appendingPathComponent("telemetria-\(f.string(from: Date())).txt")
        FileManager.default.createFile(atPath: file.path, contents: Data("Phantom F0 · telemetria dal POCO · ora dell'iPhone + riga della sonda\n".utf8))
        handle = try? FileHandle(forWritingTo: file)
        _ = try? handle?.seekToEnd()
        url = file
    }

    private func log(_ text: String) {
        pendingOut.append(Data("\(stamp.string(from: Date())) \(text)\n".utf8))
        ring.append(text)
        if ring.count > 600 { ring.removeFirst(ring.count - 600) }
        if pendingOut.count > 32_768 { flushLocked() }
    }

    private func flushLocked() {
        guard !pendingOut.isEmpty, let h = handle else { return }
        try? h.write(contentsOf: pendingOut)
        pendingOut.removeAll(keepingCapacity: true)
    }

    // MARK: rete

    private func connectLocked() {
        connection?.stateUpdateHandler = nil
        connection?.cancel()
        connection = nil
        guard isRunning, let port = NWEndpoint.Port(rawValue: 47804) else { return }
        generation &+= 1
        let gen = generation
        buffer = TelemetryLineBuffer()

        let tcp = NWProtocolTCP.Options()
        tcp.noDelay = true
        tcp.connectionTimeout = 5
        tcp.enableKeepalive = true
        tcp.keepaliveIdle = 3
        tcp.keepaliveInterval = 2
        tcp.keepaliveCount = 3
        let conn = NWConnection(host: NWEndpoint.Host(host), port: port, using: NWParameters(tls: nil, tcp: tcp))
        connection = conn
        setStatusLocked("Mi collego al POCO…")
        conn.stateUpdateHandler = { [weak self] state in
            guard let self = self, gen == self.generation else { return }
            switch state {
            case .ready:
                self.isConnected = true
                self.lastRx = Date()
                self.log("# collegato")
                self.setStatusLocked("Collegata")
                self.receiveLoop(conn, gen)
                self.publish()
            case .failed(let error):
                self.lost("errore: \(error.localizedDescription)", gen)
            case .waiting(let error):
                self.setStatusLocked("In attesa del POCO: \(error.localizedDescription)")
            default:
                break
            }
        }
        conn.start(queue: queue)
    }

    private func receiveLoop(_ conn: NWConnection, _ gen: Int) {
        conn.receive(minimumIncompleteLength: 1, maximumLength: 65_536) { [weak self] data, _, isComplete, error in
            guard let self = self, gen == self.generation else { return }
            if let data = data, !data.isEmpty { self.ingest(data) }
            if let error = error { self.lost("connessione persa: \(error.localizedDescription)", gen); return }
            if isComplete { self.lost("chiusa dal POCO", gen); return }
            self.receiveLoop(conn, gen)
        }
    }

    /// Connessione persa: lo si scrive nel registro e si riprova ogni secondo, finché la registrazione è accesa.
    private func lost(_ reason: String, _ gen: Int) {
        guard gen == generation, isRunning else { return }
        isConnected = false
        log("# \(reason)")
        setStatusLocked("Persa: riprovo ogni secondo")
        generation &+= 1
        connection?.stateUpdateHandler = nil
        connection?.cancel()
        connection = nil
        publish()
        queue.asyncAfter(deadline: .now() + 1) { [weak self] in
            guard let self = self, self.isRunning, self.connection == nil else { return }
            self.connectLocked()
        }
    }

    private func ingest(_ data: Data) {
        for line in buffer.append(data) {
            lastRx = Date()
            lines += 1
            log(line)
            switch TelemetryKind(line: line) {
            case .sample:
                if let s = TelemetrySummary.parse(line) { latest = s }
            case .stall:
                if line.contains("inizio:") { stalls += 1 }
                remember(line)
            case .blockedTask, .kernel:
                remember(line)
            default:
                break
            }
        }
    }

    private func remember(_ line: String) {
        events.append(line)
        if events.count > 40 { events.removeFirst(events.count - 40) }
    }

    // MARK: orologio interno

    private func startTimer() {
        timer?.cancel()
        let t = DispatchSource.makeTimerSource(queue: queue)
        t.schedule(deadline: .now() + 0.5, repeating: 0.5)
        t.setEventHandler { [weak self] in self?.tick() }
        t.resume()
        timer = t
    }

    private func tick() {
        guard isRunning else { return }
        let silent = Date().timeIntervalSince(lastRx)
        // Il silenzio finisce nel registro (al massimo una riga ogni 5 s): serve a datare il blocco del POCO.
        if isConnected, silent > 2, Date().timeIntervalSince(lastLoggedSilence) > 5 {
            lastLoggedSilence = Date()
            log(String(format: "# NESSUN DATO da %.1f s (la connessione è ancora aperta)", silent))
        }
        flushLocked()
        publish()
    }

    // MARK: pubblicazione verso l'interfaccia

    private func setStatus(_ text: String) { setStatusLocked(text); publish() }
    private var statusText = "Spenta"
    private func setStatusLocked(_ text: String) { statusText = text }

    private func publish() {
        let snapshot = (isRunning, isConnected, lines, Date().timeIntervalSince(lastRx), stalls, latest, Array(events.suffix(8)), url, statusText)
        DispatchQueue.main.async {
            self.running = snapshot.0
            self.connected = snapshot.1
            self.lineCount = snapshot.2
            self.silentSeconds = snapshot.1 ? snapshot.3 : 0
            self.stallCount = snapshot.4
            self.summary = snapshot.5
            self.recent = snapshot.6
            self.fileURL = snapshot.7
            self.status = snapshot.8
        }
    }
}
