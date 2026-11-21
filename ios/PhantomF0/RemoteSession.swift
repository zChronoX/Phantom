import Foundation
import UIKit
import CoreMedia
import CoreVideo
import VideoToolbox
import QuartzCore

/// Modalità di avvio della sessione remota.
enum RemoteSessionMode: Equatable {
    /// Connessione reale verso l'host Android tramite QUIC con failover automatico LAN → P2P 5G/4G e riconnessione.
    case remote(
        endpoints: [RemoteEndpointCandidate],
        certificateDer: Data,
        authToken: Data? = nil,
        codec: VideoCodec,
        autoReconnect: Bool = true
    )
    /// Prova locale su iPhone con host QUIC + encoder hardware HEVC in loopback.
    case localQuicLoopback(fps: Int, width: Int, height: Int)
}

/// Metriche live mostrate nell'overlay di controllo remoto.
struct RemoteSessionMetrics: Equatable {
    var connected = false
    var endpointLabel = ""
    var agreementSummary = ""
    var codecName = ""
    var videoWidth = 0
    var videoHeight = 0
    var fps = 0.0
    var bitrateMbps = 0.0
    var rttMs: Double?
    var queueDelayMs: Double?
    var framesDelivered: UInt64 = 0
    var framesLost: UInt64 = 0
    var hostTouchReceived: UInt64 = 0
    var hostKeyReceived: UInt64 = 0

    // Telemetria estesa (HUD espanso)
    /// Latenza dalla cattura sull'host alla consegna al decoder (ms), mediana e 95° percentile della finestra.
    var latencyP50Ms: Double?
    var latencyP95Ms: Double?
    /// Richieste di keyframe spedite dal client (cumulativo).
    var keyframeRequests: UInt64 = 0
    /// Frame scartati o non decodificati dal renderer iOS (cumulativo).
    var decoderDrops: UInt64 = 0
    /// Ultimo rapporto dell'host: `nil` se l'host è una versione che non lo manda.
    var host: HostReportInfo?
    /// Buffer anti-jitter dell'audio: `nil` finché l'audio non parte.
    var audio: JitterAudioCounters?
}

/// Coordina `QuicConnection` + `PhantomClient` (e, in loopback, l'host locale) per il controllo remoto a bassa latenza.
final class RemoteSession: ObservableObject {
    @Published private(set) var running = false
    @Published private(set) var statusText = "Pronto"
    @Published private(set) var metrics = RemoteSessionMetrics()
    @Published private(set) var videoSize = VideoDimensions.zero
    @Published private(set) var audioMuted = false
    @Published private(set) var hostScreenOff = false
    @Published private(set) var toastText: String?
    /// L'host esegue i tasti di sistema come comandi (Android 1.3+): funzionano anche in Shizuku e Accessibilità.
    @Published private(set) var systemKeysSupported = false

    var onSample: ((CMSampleBuffer, Bool) -> Void)?

    private let stateLock = NSLock()
    private var generation: UInt64 = 0
    private var stopRequested = false
    private var currentMode: RemoteSessionMode?
    private var reconnectAttempt = 0
    private var hasConnectedOnce = false
    private var activeEndpointLabel = ""
    private var activeTargetIsLan = false
    private var waitingForReconnect = false

    private var client: PhantomClient?
    private var clientEndpoint: QuicClientEndpoint?
    private var p2pClientEndpoint: P2pClientEndpoint?
    private var connection: QuicConnection?
    private var hostLoopback: LocalQuicHostRunner?
    private var pathObserver: NetworkPathObserver?
    private let audioPlayer = AudioStreamPlayer()
    private var toastClearWorkItem: DispatchWorkItem?

    private let ioQueue = DispatchQueue(label: "app.phantom.remote.io", qos: .userInteractive)
    private let recvQueue = DispatchQueue(label: "app.phantom.remote.recv", qos: .userInteractive)
    private var tickTimer: DispatchSourceTimer?

    private let sampleLock = NSLock()
    private let sampleBuilder = AnnexBSampleBuilder()
    private var touchDispatch = TouchDispatchState()
    private var decoderDrops: UInt64 = 0
    private var latestAudioCounters: JitterAudioCounters?
    private var lastReportedVideoSize = VideoDimensions.zero

    // Finestra metrica lato client (protetta da `sampleLock`)
    private var windowStart = CACurrentMediaTime()
    private var framesInWindow = 0
    private var bytesInWindow = 0
    private var latenciesInWindowMs: [Double] = []

    private static func nowUs() -> UInt64 {
        DispatchTime.now().uptimeNanoseconds / 1_000
    }

    func start(mode: RemoteSessionMode) {
        stop()

        let observer = NetworkPathObserver()
        let currentGen: UInt64 = withLock {
            generation &+= 1
            stopRequested = false
            currentMode = mode
            reconnectAttempt = 0
            hasConnectedOnce = false
            activeEndpointLabel = ""
            activeTargetIsLan = false
            waitingForReconnect = false
            pathObserver = observer
            touchDispatch.reset()
            decoderDrops = 0
            latestAudioCounters = nil
            return generation
        }
        observer.onNetworkChanged = { [weak self] isCellularOnly, isSatisfied in
            self?.handleNetworkPathChange(isCellularOnly: isCellularOnly, isSatisfied: isSatisfied)
        }
        observer.start()
        resetDecoderWindow()

        DispatchQueue.main.async {
            self.running = true
            self.metrics = RemoteSessionMetrics()
            self.videoSize = .zero
            self.statusText = "Connessione QUIC in corso…"
            UIApplication.shared.isIdleTimerDisabled = true
        }
        CrashLog.mark("RemoteSession: avvio")

        ioQueue.async { [weak self] in
            self?.runConnect(mode: mode, gen: currentGen)
        }
    }

    private func resetDecoderWindow() {
        sampleLock.lock()
        sampleBuilder.reset()
        lastReportedVideoSize = .zero
        windowStart = CACurrentMediaTime()
        framesInWindow = 0
        bytesInWindow = 0
        latenciesInWindowMs.removeAll()
        sampleLock.unlock()
    }

    func stop() {
        let (activeClient, activeConn, activeEp, activeP2p, activeHost, activeObserver):
            (PhantomClient?, QuicConnection?, QuicClientEndpoint?, P2pClientEndpoint?, LocalQuicHostRunner?, NetworkPathObserver?) = withLock {
            generation &+= 1
            stopRequested = true
            waitingForReconnect = false
            activeTargetIsLan = false
            touchDispatch.reset()
            tickTimer?.cancel()
            tickTimer = nil
            let c = client
            let q = connection
            let ep = clientEndpoint
            let p2p = p2pClientEndpoint
            let h = hostLoopback
            let obs = pathObserver
            client = nil
            connection = nil
            clientEndpoint = nil
            p2pClientEndpoint = nil
            hostLoopback = nil
            pathObserver = nil
            return (c, q, ep, p2p, h, obs)
        }

        activeObserver?.stop()
        audioPlayer.stop()
        if let c = activeClient, let q = activeConn, let closeDatagram = c.close(reason: 0) {
            try? q.sendDatagram(datagram: closeDatagram)
        }
        activeConn?.close(code: 0, reason: Data())
        _ = activeEp
        _ = activeP2p
        activeHost?.stop()

        DispatchQueue.main.async {
            self.running = false
            self.metrics.connected = false
            self.hostScreenOff = false
            UIApplication.shared.isIdleTimerDisabled = false
        }
    }

    func toggleAudioMute() {
        let next = !audioMuted
        audioMuted = next
        audioPlayer.isMuted = next
        showToast(next ? "Audio disattivato" : "Audio attivo")
    }

    func sendClipboardFromPasteboard() {
        guard let text = UIPasteboard.general.string, !text.isEmpty else {
            showToast("Appunti iPhone vuoti")
            return
        }
        let (c, q): (PhantomClient?, QuicConnection?) = withLock { (client, connection) }
        guard let c = c, let q = q else { return }
        if let datagram = c.sendClipboard(text: text) {
            try? q.sendDatagram(datagram: datagram)
            showToast("Appunti inviati ad Android")
        }
    }

    /// Tasti rapidi dell'HUD: comandi di sistema se l'host li conosce, altrimenti la vecchia sequenza HID.
    func sendQuickAction(_ action: RemoteQuickAction) {
        if systemKeysSupported {
            let command: RemoteControlCommand
            switch action {
            case .back: command = .back
            case .home: command = .home
            case .recents: command = .recents
            case .volumeUp: command = .volumeUp
            case .volumeDown: command = .volumeDown
            case .power: command = .power
            case .notifications: command = .notifications
            case .screenshot: command = .screenshot
            }
            sendControl(command)
        } else if !action.hidSequence.isEmpty {
            sendKeyEvents(action.hidSequence)
        } else {
            showToast("Aggiorna Phantom su Android per questo tasto")
        }
    }

    func sendControl(_ command: RemoteControlCommand) {
        let (c, q): (PhantomClient?, QuicConnection?) = withLock { (client, connection) }
        guard let c = c, let q = q else { return }
        if let datagram = c.sendControl(command: command) {
            try? q.sendDatagram(datagram: datagram)
            DispatchQueue.main.async {
                switch command {
                case .screenOff:
                    self.hostScreenOff = true
                    self.showToast("Schermo fisico Android spento")
                case .screenOn:
                    self.hostScreenOff = false
                    self.showToast("Schermo fisico Android acceso")
                case .wakeScreen:
                    self.hostScreenOff = false
                    self.showToast("Risveglio inviato ad Android")
                case .screenshot:
                    self.showToast("Screenshot richiesto")
                default:
                    break
                }
            }
        }
    }

    private func showToast(_ text: String) {
        DispatchQueue.main.async {
            self.toastClearWorkItem?.cancel()
            self.toastText = text
            let item = DispatchWorkItem { [weak self] in
                self?.toastText = nil
            }
            self.toastClearWorkItem = item
            DispatchQueue.main.asyncAfter(deadline: .now() + 2.2, execute: item)
        }
    }

    func updateTouches(_ fingers: [NormalizedFinger]) {
        let touchPoints = fingers.map { TouchPoint(id: $0.id, x: $0.x, y: $0.y, pressure: $0.pressure) }
        let (ver, c, q): (UInt64, PhantomClient?, QuicConnection?) = withLock {
            let v = touchDispatch.update(fingers)
            return (v, client, connection)
        }
        guard let c = c, let q = q else { return }
        if let datagram = c.touchTick(nowUs: Self.nowUs(), fingers: touchPoints) {
            withLock { touchDispatch.markSent(version: ver) }
            try? q.sendDatagram(datagram: datagram)
        }
    }

    /// Il renderer ha scartato (coda piena) o non è riuscito a decodificare un frame: i frame successivi sarebbero
    /// corrotti fino al prossimo keyframe, che con gli I-frame ogni 10 s può arrivare molto tardi. Il core scarta i
    /// frame non-key in arrivo e chiede subito un keyframe all'host (una sola richiesta alla volta).
    func requestKeyframeAfterDecoderDrop() {
        let (c, q): (PhantomClient?, QuicConnection?) = withLock {
            decoderDrops &+= 1
            return (client, connection)
        }
        guard let c = c, let q = q else { return }
        ioQueue.async {
            if let datagram = c.requestKeyframe(nowUs: Self.nowUs()) {
                try? q.sendDatagram(datagram: datagram)
            }
        }
    }

    func sendKeyEvents(_ events: [HidKeyEvent]) {
        guard !events.isEmpty else { return }
        let (c, q): (PhantomClient?, QuicConnection?) = withLock { (client, connection) }
        guard let c = c, let q = q else { return }
        let now = Self.nowUs()
        for ev in events {
            if let datagram = c.sendKey(nowUs: now, code: ev.code, down: ev.down) {
                try? q.sendDatagram(datagram: datagram)
            }
        }
    }

    func sendKey(code: UInt16, down: Bool) {
        sendKeyEvents([HidKeyEvent(code: code, down: down)])
    }

    // MARK: - Ciclo connessione

    private func runConnect(mode: RemoteSessionMode, gen: UInt64) {
        guard isGenerationActive(gen) else { return }
        withLock { waitingForReconnect = false }
        do {
            let candidates: [RemoteEndpointCandidate]
            let bindAddr: String
            let certDer: Data
            let authToken: Data?
            let preferredCodec: VideoCodec
            var loopback: LocalQuicHostRunner?

            switch mode {
            case let .remote(endpoints, certificateDer, token, codec, _):
                guard !endpoints.isEmpty else {
                    publishError("Nessun indirizzo configurato per il dispositivo")
                    return
                }
                let cellularOnly = withLock { pathObserver?.cellularOnlySnapshot ?? false }
                let nonLan = endpoints.filter {
                    if case let .directQuic(_, isLan) = $0.target { return !isLan }
                    return true
                }
                candidates = (cellularOnly && !nonLan.isEmpty) ? nonLan : endpoints
                bindAddr = "0.0.0.0:0"
                certDer = certificateDer
                authToken = token
                preferredCodec = codec

            case let .localQuicLoopback(fps, width, height):
                let runner = try LocalQuicHostRunner(fps: fps, width: width, height: height)
                runner.start()
                candidates = [
                    RemoteEndpointCandidate(
                        target: .directQuic(address: runner.address, isLan: true),
                        label: "Loopback",
                        timeoutMs: 5_000
                    ),
                ]
                bindAddr = "127.0.0.1:0"
                certDer = runner.certificateDer
                authToken = nil
                preferredCodec = .h265
                loopback = runner
            }

            guard isGenerationActive(gen) else {
                loopback?.stop()
                return
            }

            var connectedDirectEp: QuicClientEndpoint?
            var connectedP2pEp: P2pClientEndpoint?
            var connectedLink: QuicConnection?
            var chosenLabel = ""
            var chosenIsLan = false
            var attemptErrors: [String] = []

            for candidate in candidates {
                guard isGenerationActive(gen) else {
                    loopback?.stop()
                    return
                }
                DispatchQueue.main.async {
                    self.statusText = "Connessione (\(candidate.label)): \(candidate.displayAddress)…"
                }
                do {
                    switch candidate.target {
                    case let .directQuic(address, isLan):
                        guard !certDer.isEmpty else {
                            throw QuicTransportError.EmptyCertificate
                        }
                        let ep = try QuicClientEndpoint.bind(addr: bindAddr)
                        let link = try ep.connect(
                            addr: address,
                            serverName: defaultQuicServerName(),
                            pinnedCertificateDer: certDer,
                            timeoutMs: candidate.timeoutMs
                        )
                        connectedDirectEp = ep
                        connectedLink = link
                        chosenLabel = candidate.label
                        chosenIsLan = isLan

                    case let .p2pIroh(endpointId):
                        let ep = try P2pClientEndpoint.bind(onlineTimeoutMs: 2_500)
                        let link = try ep.connect(
                            remoteEndpointId: endpointId,
                            timeoutMs: candidate.timeoutMs
                        )
                        connectedP2pEp = ep
                        connectedLink = link
                        chosenLabel = candidate.label
                        chosenIsLan = false
                    }
                    break
                } catch {
                    attemptErrors.append("\(candidate.label): \(error)")
                }
            }

            guard let conn = connectedLink else {
                loopback?.stop()
                let desc = attemptErrors.isEmpty ? "timeout" : attemptErrors.joined(separator: " · ")
                handleDisconnect(reason: "Connessione QUIC fallita: \(desc)", orderly: false, gen: gen)
                return
            }

            var settings = defaultSessionSettings()
            settings.codec = preferredCodec
            settings.minBitrateBps = 1_000_000
            settings.nonce = Data((0..<16).map { _ in UInt8.random(in: 0...255) })
            if let maxDgram = conn.maxDatagramSize() {
                settings.fragSize = videoPayloadForDatagram(maxDatagramSize: maxDgram)
            }
            let phantomClient = PhantomClient.newWithAuth(settings: settings, authToken: authToken ?? Data())
            audioPlayer.start()

            let installed = withLock { () -> Bool in
                guard generation == gen && !stopRequested else { return false }
                self.client = phantomClient
                self.clientEndpoint = connectedDirectEp
                self.p2pClientEndpoint = connectedP2pEp
                self.connection = conn
                self.hostLoopback = loopback
                self.activeEndpointLabel = chosenLabel
                self.activeTargetIsLan = chosenIsLan
                self.reconnectAttempt = 0
                self.hasConnectedOnce = true
                return true
            }
            guard installed else {
                conn.close(code: 0, reason: Data())
                loopback?.stop()
                return
            }

            DispatchQueue.main.async {
                self.statusText = "Apertura sessione (\(chosenLabel))…"
            }

            startLoops(client: phantomClient, conn: conn, gen: gen)
        } catch {
            handleDisconnect(reason: "Connessione QUIC fallita: \(error)", orderly: false, gen: gen)
        }
    }

    private func startLoops(client: PhantomClient, conn: QuicConnection, gen: UInt64) {
        // Thread lettore dedicato: `recvDatagram` non blocca gli invii sullo stesso `QuicConnection`.
        recvQueue.async { [weak self] in
            guard let self = self else { return }
            while self.isGenerationActive(gen) && !conn.isClosed() && !client.isClosed() {
                do {
                    let datagram = try conn.recvDatagram(maxBytes: 2048, timeoutMs: 25)
                    let replies = client.onDatagram(nowUs: Self.nowUs(), data: datagram)
                    for reply in replies {
                        try? conn.sendDatagram(datagram: reply)
                    }
                    self.drainDeliveredFrames(client: client, gen: gen)
                    self.drainAudioAndClipboard(client: client, gen: gen)
                } catch QuicTransportError.Timeout {
                    continue
                } catch {
                    if self.isGenerationActive(gen) {
                        self.handleDisconnect(reason: "Collegamento QUIC interrotto: \(error)", orderly: false, gen: gen)
                    }
                    break
                }
            }
        }

        // Timer a 250 Hz (allineato al tocco a 240 Hz) per `tick`, stato del tocco e telemetria.
        let timer = DispatchSource.makeTimerSource(flags: .strict, queue: ioQueue)
        timer.schedule(deadline: .now(), repeating: .milliseconds(4), leeway: .microseconds(500))
        timer.setEventHandler { [weak self] in
            self?.onTick(client: client, conn: conn, gen: gen)
        }
        withLock {
            if generation == gen && !stopRequested {
                tickTimer = timer
                timer.resume()
            } else {
                timer.cancel()
            }
        }
    }

    private func onTick(client: PhantomClient, conn: QuicConnection, gen: UInt64) {
        guard isGenerationActive(gen) else { return }

        if client.isClosed() {
            let reasonText = SessionAgreement.closeText(client.closeReason())
            handleDisconnect(reason: "Sessione chiusa: \(reasonText)", orderly: true, gen: gen)
            return
        }
        if conn.isClosed() {
            handleDisconnect(reason: "Connessione QUIC chiusa dal trasporto", orderly: false, gen: gen)
            return
        }

        let now = Self.nowUs()
        for datagram in client.tick(nowUs: now) {
            try? conn.sendDatagram(datagram: datagram)
        }

        let touchSnapshot: (ver: UInt64, fingers: [TouchPoint])? = withLock {
            guard touchDispatch.shouldSendOnTick else { return nil }
            let pts = touchDispatch.latestFingers.map { TouchPoint(id: $0.id, x: $0.x, y: $0.y, pressure: $0.pressure) }
            return (touchDispatch.version, pts)
        }
        if let snap = touchSnapshot, let touchDgram = client.touchTick(nowUs: now, fingers: snap.fingers) {
            withLock { touchDispatch.markSent(version: snap.ver) }
            try? conn.sendDatagram(datagram: touchDgram)
        }

        drainDeliveredFrames(client: client, gen: gen)
        drainAudioAndClipboard(client: client, gen: gen)
        refreshSessionAndMetrics(client: client, nowUs: now, gen: gen)
    }

    private func drainAudioAndClipboard(client: PhantomClient, gen: UInt64) {
        guard isGenerationActive(gen) else { return }
        for audio in client.takeAudio() {
            audioPlayer.enqueue(audio)
        }
        if let clip = client.takeClipboard(), !clip.isEmpty {
            DispatchQueue.main.async {
                UIPasteboard.general.string = clip
            }
            showToast("Appunti ricevuti da Android")
        }
    }

    private func drainDeliveredFrames(client: PhantomClient, gen: UInt64) {
        let frames = client.takeFrames()
        guard !frames.isEmpty, isGenerationActive(gen) else { return }
        var readySamples: [(CMSampleBuffer, Bool)] = []
        var updatedSize: VideoDimensions?

        sampleLock.lock()
        for f in frames {
            bytesInWindow += f.data.count
            framesInWindow += 1
            if let lat = f.latencyUs, lat >= 0, lat < 5_000_000 {
                latenciesInWindowMs.append(Double(lat) / 1000.0)
            }
            if let built = sampleBuilder.buildSample(from: f.data, codec: f.codec) {
                readySamples.append((built.sampleBuffer, built.formatChanged))
                if built.dimensions.isValid && built.dimensions != lastReportedVideoSize {
                    lastReportedVideoSize = built.dimensions
                    updatedSize = built.dimensions
                }
            }
        }
        sampleLock.unlock()

        if let size = updatedSize {
            DispatchQueue.main.async { self.videoSize = size }
        }
        for (sb, flush) in readySamples {
            onSample?(sb, flush)
        }
    }

    private func refreshSessionAndMetrics(client: PhantomClient, nowUs: UInt64, gen: UInt64) {
        sampleLock.lock()
        let nowMedia = CACurrentMediaTime()
        let elapsed = nowMedia - windowStart
        guard elapsed >= 0.5 else {
            sampleLock.unlock()
            return
        }
        let fps = Double(framesInWindow) / elapsed
        let mbps = Double(bytesInWindow) * 8.0 / 1_000_000.0 / elapsed
        let videoDims = sampleBuilder.dimensions
        let latency = BenchSummary(latenciesInWindowMs)
        windowStart = nowMedia
        framesInWindow = 0
        bytesInWindow = 0
        latenciesInWindowMs.removeAll(keepingCapacity: true)
        sampleLock.unlock()

        // I contatori audio arrivano dalla coda dell'audio: si usa l'ultimo valore (al massimo mezzo secondo vecchio).
        audioPlayer.counters { [weak self] c in
            guard let self = self else { return }
            self.withLock { self.latestAudioCounters = c }
        }

        let negotiated = client.negotiated()
        let counters = client.counters(nowUs: nowUs)
        let (hostCounters, epLabel, drops, audioCounters): (HostCounters?, String, UInt64, JitterAudioCounters?) = withLock {
            (hostLoopback?.hostCounters(), activeEndpointLabel, decoderDrops, latestAudioCounters)
        }

        var snap = RemoteSessionMetrics()
        snap.connected = negotiated != nil && !client.isClosed()
        snap.endpointLabel = epLabel
        snap.fps = fps
        snap.bitrateMbps = mbps
        snap.rttMs = counters.rttUs.map { Double($0) / 1000.0 }
        snap.queueDelayMs = counters.queueDelayUs.map { Double($0) / 1000.0 }
        snap.framesDelivered = counters.framesOk
        snap.framesLost = counters.framesLost
        snap.videoWidth = videoDims.width
        snap.videoHeight = videoDims.height
        if latency.count > 0 {
            snap.latencyP50Ms = latency.p50
            snap.latencyP95Ms = latency.p95
        }
        snap.keyframeRequests = counters.keyframeRequests
        snap.decoderDrops = drops
        snap.host = client.hostReport()
        snap.audio = audioCounters
        if let hc = hostCounters {
            snap.hostTouchReceived = hc.touchStates
            snap.hostKeyReceived = hc.keyEvents
        }
        if let n = negotiated {
            let agreement = SessionAgreement(
                proto: n.proto,
                hevc: capsContains(caps: n.caps, capability: .h265),
                avc: capsContains(caps: n.caps, capability: .h264),
                parity: capsContains(caps: n.caps, capability: .xorParity),
                retransmit: capsContains(caps: n.caps, capability: .nack),
                keyframeRequest: capsContains(caps: n.caps, capability: .keyframeRequest),
                queueDelay: capsContains(caps: n.caps, capability: .queueDelay),
                peerNonce: n.peerNonce
            )
            snap.agreementSummary = agreement.summary
            snap.codecName = agreement.videoCodec ?? "n/d"
        }

        let systemKeys = negotiated.map { capsContains(caps: $0.caps, capability: .systemKeys) } ?? false
        guard isGenerationActive(gen) else { return }
        DispatchQueue.main.async {
            if self.systemKeysSupported != systemKeys {
                self.systemKeysSupported = systemKeys
            }
            self.metrics = snap
            if snap.connected {
                self.statusText = "Connesso (\(snap.codecName))"
            }
        }
    }

    private func handleNetworkPathChange(isCellularOnly: Bool, isSatisfied: Bool) {
        enum PathAction {
            case none
            case migrateP2p(P2pClientEndpoint)
            case triggerFastReconnect(gen: UInt64, immediate: Bool)
            case retryNow(mode: RemoteSessionMode, nextGen: UInt64)
        }

        let action: PathAction = withLock {
            guard !stopRequested, let mode = currentMode, case .remote = mode else { return .none }
            if waitingForReconnect && isSatisfied {
                waitingForReconnect = false
                reconnectAttempt = 0
                generation &+= 1
                return .retryNow(mode: mode, nextGen: generation)
            }
            if let p2p = p2pClientEndpoint {
                return .migrateP2p(p2p)
            }
            if activeTargetIsLan && (isCellularOnly || !isSatisfied) {
                return .triggerFastReconnect(gen: generation, immediate: isCellularOnly && isSatisfied)
            }
            return .none
        }

        switch action {
        case .none:
            break
        case let .migrateP2p(p2p):
            DispatchQueue.global(qos: .userInitiated).async {
                p2p.networkChange()
            }
        case let .triggerFastReconnect(gen, immediate):
            handleDisconnect(reason: "Cambio rete (Wi-Fi ↔ 5G/4G)", orderly: false, gen: gen, immediate: immediate)
        case let .retryNow(mode, nextGen):
            DispatchQueue.main.async {
                self.statusText = "Nuova rete rilevata · riconnessione…"
            }
            ioQueue.async { [weak self] in
                self?.runConnect(mode: mode, gen: nextGen)
            }
        }
    }

    private func handleDisconnect(reason: String, orderly: Bool, gen: UInt64, immediate: Bool = false) {
        let result: (
            conn: QuicConnection?,
            directEp: QuicClientEndpoint?,
            p2pEp: P2pClientEndpoint?,
            loopback: LocalQuicHostRunner?,
            retry: (retryMode: RemoteSessionMode, nextGen: UInt64, attempt: Int, delay: Double)?
        )? = withLock {
            guard generation == gen && !stopRequested else { return nil }
            tickTimer?.cancel()
            tickTimer = nil
            let q = connection
            let ep = clientEndpoint
            let p2p = p2pClientEndpoint
            let lb = hostLoopback
            client = nil
            connection = nil
            clientEndpoint = nil
            p2pClientEndpoint = nil
            activeTargetIsLan = false
            hostLoopback = nil
            touchDispatch.reset()

            guard !orderly,
                  hasConnectedOnce,
                  let mode = currentMode,
                  case let .remote(_, _, _, _, autoReconnect) = mode,
                  autoReconnect,
                  reconnectAttempt < ReconnectBackoff.maxAttempts else {
                return (q, ep, p2p, lb, nil)
            }
            reconnectAttempt += 1
            waitingForReconnect = true
            generation &+= 1
            let delay = immediate ? 0.05 : ReconnectBackoff.delaySeconds(attempt: reconnectAttempt)
            return (q, ep, p2p, lb, (mode, generation, reconnectAttempt, delay))
        }

        guard let outcome = result else { return }
        outcome.conn?.close(code: 0, reason: Data())
        outcome.loopback?.stop()
        _ = outcome.directEp
        _ = outcome.p2pEp

        audioPlayer.stop()
        guard let retry = outcome.retry else {
            publishError(reason)
            return
        }

        resetDecoderWindow()
        DispatchQueue.main.async {
            self.metrics.connected = false
            self.statusText = "Cambio rete / riconnessione (\(retry.attempt)/\(ReconnectBackoff.maxAttempts))…"
        }

        ioQueue.asyncAfter(deadline: .now() + retry.delay) { [weak self] in
            self?.runConnect(mode: retry.retryMode, gen: retry.nextGen)
        }
    }

    private func publishError(_ message: String) {
        stop()
        DispatchQueue.main.async {
            self.statusText = message
        }
    }

    private func isGenerationActive(_ gen: UInt64) -> Bool {
        withLock { generation == gen && !stopRequested }
    }

    private func withLock<T>(_ body: () -> T) -> T {
        stateLock.lock()
        defer { stateLock.unlock() }
        return body()
    }
}

/// Host QUIC + encoder HEVC in loopback sullo stesso iPhone per provare l'intera UX senza POCO.
private final class LocalQuicHostRunner {
    let address: String
    let certificateDer: Data

    private let endpoint: QuicHostEndpoint
    private let fps: Int
    private let width: Int
    private let height: Int

    private let stopLock = NSLock()
    private var stopped = false
    private var host: PhantomHost?
    private var link: QuicConnection?
    private var lastReceivedFingers: [TouchPoint] = []

    init(fps: Int, width: Int, height: Int) throws {
        self.fps = fps
        self.width = width
        self.height = height
        endpoint = try QuicHostEndpoint.bind(addr: "127.0.0.1:0")
        address = try endpoint.localAddr()
        certificateDer = endpoint.certificateDer()
    }

    func start() {
        DispatchQueue.global(qos: .userInteractive).async { [weak self] in
            self?.run()
        }
    }

    func stop() {
        stopLock.lock()
        stopped = true
        let l = link
        link = nil
        stopLock.unlock()
        l?.close(code: 0, reason: Data())
    }

    func hostCounters() -> HostCounters? {
        stopLock.lock()
        let h = host
        stopLock.unlock()
        return h?.counters()
    }

    private var isStopped: Bool {
        stopLock.lock()
        defer { stopLock.unlock() }
        return stopped
    }

    private func run() {
        guard let conn = try? endpoint.accept(timeoutMs: 5_000) else { return }
        stopLock.lock()
        if stopped {
            stopLock.unlock()
            conn.close(code: 0, reason: Data())
            return
        }
        link = conn

        var settings = defaultSessionSettings()
        settings.codec = .h265
        settings.nonce = Data((0..<16).map { _ in UInt8.random(in: 0...255) })
        if let maxDgram = conn.maxDatagramSize() {
            settings.fragSize = videoPayloadForDatagram(maxDatagramSize: maxDgram)
        }
        let phantomHost = PhantomHost(settings: settings)
        host = phantomHost
        stopLock.unlock()

        let recvQueue = DispatchQueue(label: "app.phantom.loopback.host.recv", qos: .userInteractive)
        recvQueue.async { [weak self] in
            guard let self = self else { return }
            while !self.isStopped && !conn.isClosed() && !phantomHost.isClosed() {
                do {
                    let d = try conn.recvDatagram(maxBytes: 2048, timeoutMs: 20)
                    let now = DispatchTime.now().uptimeNanoseconds / 1_000
                    for reply in phantomHost.onDatagram(nowUs: now, data: d) {
                        try? conn.sendDatagram(datagram: reply)
                    }
                    // Conserva l'ultimo stato del tocco per disegnarlo nel video di loopback e svuota i tasti
                    while let touchState = phantomHost.takeTouch() {
                        self.stopLock.lock()
                        self.lastReceivedFingers = touchState.fingers
                        self.stopLock.unlock()
                    }
                    while phantomHost.takeKey() != nil {}
                } catch QuicTransportError.Timeout {
                    continue
                } catch {
                    break
                }
            }
        }

        // Attende l'apertura della sessione prima di avviare l'encoder
        let waitDeadline = DispatchTime.now().uptimeNanoseconds + 3_000_000_000
        while !isStopped && phantomHost.negotiated() == nil && DispatchTime.now().uptimeNanoseconds < waitDeadline {
            usleep(5_000)
        }
        guard !isStopped, phantomHost.negotiated() != nil else { return }
        streamSyntheticHevc(host: phantomHost, conn: conn)
    }

    private func streamSyntheticHevc(host: PhantomHost, conn: QuicConnection) {
        var enc: VTCompressionSession?
        let spec: [CFString: Any] = [kVTVideoEncoderSpecification_EnableLowLatencyRateControl: true]
        var st = VTCompressionSessionCreate(
            allocator: nil,
            width: Int32(width),
            height: Int32(height),
            codecType: kCMVideoCodecType_HEVC,
            encoderSpecification: spec as CFDictionary,
            imageBufferAttributes: nil,
            compressedDataAllocator: nil,
            outputCallback: nil,
            refcon: nil,
            compressionSessionOut: &enc
        )
        if st != noErr || enc == nil {
            st = VTCompressionSessionCreate(
                allocator: nil,
                width: Int32(width),
                height: Int32(height),
                codecType: kCMVideoCodecType_HEVC,
                encoderSpecification: nil,
                imageBufferAttributes: nil,
                compressedDataAllocator: nil,
                outputCallback: nil,
                refcon: nil,
                compressionSessionOut: &enc
            )
        }
        guard st == noErr, let encoder = enc else { return }
        defer { VTCompressionSessionInvalidate(encoder) }

        VTSessionSetProperty(encoder, key: kVTCompressionPropertyKey_RealTime, value: kCFBooleanTrue)
        VTSessionSetProperty(encoder, key: kVTCompressionPropertyKey_AllowFrameReordering, value: kCFBooleanFalse)
        VTSessionSetProperty(encoder, key: kVTCompressionPropertyKey_ExpectedFrameRate, value: NSNumber(value: fps))
        VTSessionSetProperty(encoder, key: kVTCompressionPropertyKey_AverageBitRate, value: NSNumber(value: host.targetBitrateBps()))
        VTSessionSetProperty(encoder, key: kVTCompressionPropertyKey_MaxFrameDelayCount, value: NSNumber(value: 0))
        VTCompressionSessionPrepareToEncodeFrames(encoder)

        let attrs: [CFString: Any] = [
            kCVPixelBufferPixelFormatTypeKey: kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange,
            kCVPixelBufferWidthKey: width,
            kCVPixelBufferHeightKey: height,
            kCVPixelBufferIOSurfacePropertiesKey: [String: Any](),
        ]
        var pool: CVPixelBufferPool?
        CVPixelBufferPoolCreate(nil, nil, attrs as CFDictionary, &pool)
        guard let bufferPool = pool else { return }

        var appliedBitrate = Int(host.targetBitrateBps())
        let lumaPattern = Self.makeLumaBackground(width: width, height: height + 96)
        var frameIndex = 0
        let frameIntervalNs = UInt64(1_000_000_000 / max(1, fps))
        let bitrateStep = max(1, fps / 2)
        let duration = CMTime(value: 1, timescale: CMTimeScale(fps))
        var nextFrameNs = DispatchTime.now().uptimeNanoseconds

        while !isStopped && !conn.isClosed() && !host.isClosed() {
            let nowNs = DispatchTime.now().uptimeNanoseconds
            if nowNs < nextFrameNs {
                let remNs = nextFrameNs - nowNs
                if remNs > 500_000 {
                    usleep(UInt32((remNs - 200_000) / 1_000))
                }
                while DispatchTime.now().uptimeNanoseconds < nextFrameNs && !isStopped {}
            } else if nowNs - nextFrameNs > frameIntervalNs * 2 {
                nextFrameNs = nowNs
            }
            nextFrameNs &+= frameIntervalNs

            if frameIndex % bitrateStep == 0 {
                let target = Int(host.targetBitrateBps())
                if appliedBitrate > 0 && abs(target - appliedBitrate) * 100 > appliedBitrate * 8 {
                    VTSessionSetProperty(encoder, key: kVTCompressionPropertyKey_AverageBitRate, value: NSNumber(value: target))
                    appliedBitrate = target
                }
            }

            let frameStart = DispatchTime.now().uptimeNanoseconds
            var pbOut: CVPixelBuffer?
            if CVPixelBufferPoolCreatePixelBuffer(nil, bufferPool, &pbOut) == kCVReturnSuccess, let pb = pbOut {
                fillSyntheticFrame(pb, index: frameIndex, lumaBackground: lumaPattern)
                let captureUs = frameStart / 1_000
                let forceKey = frameIndex == 0 || host.wantsKeyframe(nowUs: captureUs)
                let props: CFDictionary? = forceKey ? [kVTEncodeFrameOptionKey_ForceKeyFrame: true] as CFDictionary : nil
                let pts = CMTime(value: CMTimeValue(frameIndex), timescale: CMTimeScale(fps))
                VTCompressionSessionEncodeFrame(
                    encoder,
                    imageBuffer: pb,
                    presentationTimeStamp: pts,
                    duration: duration,
                    frameProperties: props,
                    infoFlagsOut: nil
                ) { [weak self] status, _, sample in
                    guard let self = self, !self.isStopped else { return }
                    guard status == noErr, let sb = sample,
                          let annex = Self.annexB(from: sb) else { return }
                    let key = Self.isKeyFrame(sb)
                    for d in host.sendFrame(key: key, captureTsUs: captureUs, data: annex) {
                        try? conn.sendDatagram(datagram: d)
                    }
                }
                frameIndex &+= 1
            }
        }
        VTCompressionSessionCompleteFrames(encoder, untilPresentationTimeStamp: .invalid)
    }

    private static func makeLumaBackground(width: Int, height: Int) -> [UInt8] {
        var buf = [UInt8](repeating: 0, count: width * height)
        for row in 0..<height {
            let v = UInt8(32 + (row % 96))
            buf.withUnsafeMutableBytes { raw in
                if let base = raw.baseAddress {
                    memset(base + row * width, Int32(v), width)
                }
            }
        }
        return buf
    }

    private func fillSyntheticFrame(_ pb: CVPixelBuffer, index: Int, lumaBackground: [UInt8]) {
        stopLock.lock()
        let fingers = lastReceivedFingers
        stopLock.unlock()

        CVPixelBufferLockBaseAddress(pb, [])
        defer { CVPixelBufferUnlockBaseAddress(pb, []) }
        let yWidth = CVPixelBufferGetWidthOfPlane(pb, 0)
        let yHeight = CVPixelBufferGetHeightOfPlane(pb, 0)

        if let yBase = CVPixelBufferGetBaseAddressOfPlane(pb, 0) {
            let stride = CVPixelBufferGetBytesPerRowOfPlane(pb, 0)
            let barTop = (index * 16) % max(1, yHeight - 80)
            let shift = (index * 2) % 96
            let copyW = min(yWidth, width)
            lumaBackground.withUnsafeBufferPointer { bg in
                guard let bgBase = bg.baseAddress else { return }
                for row in 0..<yHeight {
                    if row >= barTop && row < barTop + 80 {
                        memset(yBase + row * stride, 210, yWidth)
                    } else {
                        memcpy(yBase + row * stride, bgBase + (row + shift) * width, copyW)
                    }
                }
            }
            for finger in fingers {
                let cx = Int((Double(finger.x) / 65535.0) * Double(max(0, yWidth - 1)))
                let cy = Int((Double(finger.y) / 65535.0) * Double(max(0, yHeight - 1)))
                let x0 = max(0, cx - 24)
                let x1 = min(yWidth, cx + 24)
                let y0 = max(0, cy - 24)
                let y1 = min(yHeight, cy + 24)
                if x1 > x0 {
                    for row in y0..<y1 {
                        memset(yBase + row * stride + x0, 245, x1 - x0)
                    }
                }
            }
        }
        if let uvBase = CVPixelBufferGetBaseAddressOfPlane(pb, 1) {
            let uvH = CVPixelBufferGetHeightOfPlane(pb, 1)
            let uvW = CVPixelBufferGetWidthOfPlane(pb, 1)
            let stride = CVPixelBufferGetBytesPerRowOfPlane(pb, 1)
            memset(uvBase, Int32(96 + (index % 64)), uvH * stride)

            let uvBytes = uvBase.assumingMemoryBound(to: UInt8.self)
            for finger in fingers {
                let cx = Int((Double(finger.x) / 65535.0) * Double(max(0, yWidth - 1)))
                let cy = Int((Double(finger.y) / 65535.0) * Double(max(0, yHeight - 1)))
                let ux0 = max(0, (cx - 24) / 2)
                let ux1 = min(uvW, (cx + 24 + 1) / 2)
                let uy0 = max(0, (cy - 24) / 2)
                let uy1 = min(uvH, (cy + 24 + 1) / 2)
                for row in uy0..<uy1 {
                    for col in ux0..<ux1 {
                        let offset = row * stride + col * 2
                        uvBytes[offset] = 40
                        uvBytes[offset + 1] = 220
                    }
                }
            }
        }
    }

    private static func isKeyFrame(_ sb: CMSampleBuffer) -> Bool {
        let attachments = CMSampleBufferGetSampleAttachmentsArray(sb, createIfNecessary: false) as? [[CFString: Any]]
        let notSync = attachments?.first?[kCMSampleAttachmentKey_NotSync] as? Bool ?? false
        return !notSync
    }

    private static func annexB(from sb: CMSampleBuffer) -> Data? {
        guard let fd = CMSampleBufferGetFormatDescription(sb), let block = CMSampleBufferGetDataBuffer(sb) else { return nil }
        var count = 0
        var nalLength: Int32 = 4
        let probe = CMVideoFormatDescriptionGetHEVCParameterSetAtIndex(
            fd,
            parameterSetIndex: 0,
            parameterSetPointerOut: nil,
            parameterSetSizeOut: nil,
            parameterSetCountOut: &count,
            nalUnitHeaderLengthOut: &nalLength
        )
        guard probe == noErr else { return nil }
        let size = CMBlockBufferGetDataLength(block)
        var bytes = [UInt8](repeating: 0, count: size)
        guard CMBlockBufferCopyDataBytes(block, atOffset: 0, dataLength: size, destination: &bytes) == kCMBlockBufferNoErr else {
            return nil
        }
        var out = Data()
        if isKeyFrame(sb) {
            var sets: [Data] = []
            for i in 0..<count {
                var pointer: UnsafePointer<UInt8>?
                var setSize = 0
                let st = CMVideoFormatDescriptionGetHEVCParameterSetAtIndex(
                    fd,
                    parameterSetIndex: i,
                    parameterSetPointerOut: &pointer,
                    parameterSetSizeOut: &setSize,
                    parameterSetCountOut: nil,
                    nalUnitHeaderLengthOut: nil
                )
                if st == noErr, let p = pointer { sets.append(Data(bytes: p, count: setSize)) }
            }
            out.append(AnnexBConvert.parameterSets(sets))
        }
        out.append(AnnexBConvert.fromLengthPrefixed(Data(bytes), lengthSize: Int(nalLength)))
        return out.isEmpty ? nil : out
    }
}
