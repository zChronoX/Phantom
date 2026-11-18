import Foundation
import UIKit
import CoreMedia
import CoreVideo
import VideoToolbox
import QuartzCore

/// Modalità di avvio della sessione remota.
enum RemoteSessionMode: Equatable {
    /// Connessione reale verso l'host Android tramite QUIC con certificato DER fissato.
    case remote(address: String, certificateDer: Data, codec: VideoCodec)
    /// Prova locale su iPhone con host QUIC + encoder hardware HEVC in loopback.
    case localQuicLoopback(fps: Int, width: Int, height: Int)
}

/// Metriche live mostrate nell'overlay di controllo remoto.
struct RemoteSessionMetrics: Equatable {
    var connected = false
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
}

/// Coordina `QuicConnection` + `PhantomClient` (e, in loopback, l'host locale) per il controllo remoto a bassa latenza.
final class RemoteSession: ObservableObject {
    @Published private(set) var running = false
    @Published private(set) var statusText = "Pronto"
    @Published private(set) var metrics = RemoteSessionMetrics()
    @Published private(set) var videoSize: (width: Int, height: Int) = (0, 0)

    var onSample: ((CMSampleBuffer) -> Void)?

    private let stateLock = NSLock()
    private var generation: UInt64 = 0
    private var stopRequested = false

    private var client: PhantomClient?
    private var connection: QuicConnection?
    private var hostLoopback: LocalQuicHostRunner?

    private let ioQueue = DispatchQueue(label: "app.phantom.remote.io", qos: .userInteractive)
    private let recvQueue = DispatchQueue(label: "app.phantom.remote.recv", qos: .userInteractive)
    private var tickTimer: DispatchSourceTimer?

    private let sampleLock = NSLock()
    private let sampleBuilder = AnnexBSampleBuilder()
    private var latestFingers: [NormalizedFinger] = []
    private var lastReportedVideoSize: (width: Int, height: Int) = (0, 0)

    // Finestra metrica lato client (protetta da `sampleLock`)
    private var windowStart = CACurrentMediaTime()
    private var framesInWindow = 0
    private var bytesInWindow = 0

    private static func nowUs() -> UInt64 {
        DispatchTime.now().uptimeNanoseconds / 1_000
    }

    func start(mode: RemoteSessionMode) {
        stop()

        let currentGen: UInt64 = withLock {
            generation &+= 1
            stopRequested = false
            latestFingers = []
            return generation
        }
        sampleLock.lock()
        sampleBuilder.reset()
        lastReportedVideoSize = (0, 0)
        windowStart = CACurrentMediaTime()
        framesInWindow = 0
        bytesInWindow = 0
        sampleLock.unlock()

        DispatchQueue.main.async {
            self.running = true
            self.metrics = RemoteSessionMetrics()
            self.videoSize = (0, 0)
            self.statusText = "Connessione QUIC in corso…"
        }
        UIApplication.shared.isIdleTimerDisabled = true
        CrashLog.mark("RemoteSession: avvio")

        ioQueue.async { [weak self] in
            self?.runConnect(mode: mode, gen: currentGen)
        }
    }

    func stop() {
        let (activeClient, activeConn, activeHost): (PhantomClient?, QuicConnection?, LocalQuicHostRunner?) = withLock {
            generation &+= 1
            stopRequested = true
            tickTimer?.cancel()
            tickTimer = nil
            let c = client
            let q = connection
            let h = hostLoopback
            client = nil
            connection = nil
            hostLoopback = nil
            return (c, q, h)
        }

        if let c = activeClient, let q = activeConn, let closeDatagram = c.close(reason: 0) {
            try? q.sendDatagram(datagram: closeDatagram)
        }
        activeConn?.close(code: 0, reason: Data())
        activeHost?.stop()

        DispatchQueue.main.async {
            self.running = false
            self.metrics.connected = false
            UIApplication.shared.isIdleTimerDisabled = false
        }
    }

    func updateTouches(_ fingers: [NormalizedFinger]) {
        let touchPoints = fingers.map { TouchPoint(id: $0.id, x: $0.x, y: $0.y, pressure: $0.pressure) }
        let (c, q): (PhantomClient?, QuicConnection?) = withLock {
            latestFingers = fingers
            return (client, connection)
        }
        guard let c = c, let q = q else { return }
        if let datagram = c.touchTick(nowUs: Self.nowUs(), fingers: touchPoints) {
            try? q.sendDatagram(datagram: datagram)
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
        do {
            let targetAddr: String
            let certDer: Data
            let preferredCodec: VideoCodec
            var loopback: LocalQuicHostRunner?

            switch mode {
            case let .remote(address, certificateDer, codec):
                guard let normalized = EndpointParser.normalizeAddress(address) else {
                    publishError("Indirizzo non valido (es. 192.168.1.118:47800)")
                    return
                }
                guard !certificateDer.isEmpty else {
                    publishError("Certificato dell'host mancante")
                    return
                }
                targetAddr = normalized
                certDer = certificateDer
                preferredCodec = codec

            case let .localQuicLoopback(fps, width, height):
                let runner = try LocalQuicHostRunner(fps: fps, width: width, height: height)
                runner.start()
                targetAddr = runner.address
                certDer = runner.certificateDer
                preferredCodec = .h265
                loopback = runner
            }

            guard isGenerationActive(gen) else {
                loopback?.stop()
                return
            }

            let clientEp = try QuicClientEndpoint.bind(addr: "0.0.0.0:0")
            let conn = try clientEp.connect(
                addr: targetAddr,
                serverName: defaultQuicServerName(),
                pinnedCertificateDer: certDer,
                timeoutMs: 5_000
            )

            var settings = defaultSessionSettings()
            settings.codec = preferredCodec
            settings.nonce = Data((0..<16).map { _ in UInt8.random(in: 0...255) })
            if let maxDgram = conn.maxDatagramSize() {
                settings.fragSize = videoPayloadForDatagram(maxDatagramSize: maxDgram)
            }
            let phantomClient = PhantomClient(settings: settings)

            let installed = withLock { () -> Bool in
                guard generation == gen && !stopRequested else { return false }
                self.client = phantomClient
                self.connection = conn
                self.hostLoopback = loopback
                return true
            }
            guard installed else {
                conn.close(code: 0, reason: Data())
                loopback?.stop()
                return
            }

            DispatchQueue.main.async {
                self.statusText = "Apertura sessione (Hello/HelloAck)…"
            }

            startLoops(client: phantomClient, conn: conn, gen: gen)
        } catch {
            publishError("Connessione QUIC fallita: \(error)")
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
                } catch QuicTransportError.Timeout {
                    continue
                } catch {
                    if self.isGenerationActive(gen) {
                        self.publishError("Collegamento QUIC interrotto: \(error)")
                    }
                    break
                }
            }
        }

        // Timer ad alta frequenza per `tick`, stato del tocco e telemetria.
        let timer = DispatchSource.makeTimerSource(flags: .strict, queue: ioQueue)
        timer.schedule(deadline: .now(), repeating: .milliseconds(2), leeway: .microseconds(250))
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

        if client.isClosed() || conn.isClosed() {
            let reasonText = SessionAgreement.closeText(client.closeReason())
            publishError("Sessione chiusa: \(reasonText)")
            return
        }

        let now = Self.nowUs()
        for datagram in client.tick(nowUs: now) {
            try? conn.sendDatagram(datagram: datagram)
        }

        let fingers: [TouchPoint] = withLock {
            latestFingers.map { TouchPoint(id: $0.id, x: $0.x, y: $0.y, pressure: $0.pressure) }
        }
        if !fingers.isEmpty, let touchDgram = client.touchTick(nowUs: now, fingers: fingers) {
            try? conn.sendDatagram(datagram: touchDgram)
        }

        drainDeliveredFrames(client: client, gen: gen)
        refreshSessionAndMetrics(client: client, nowUs: now, gen: gen)
    }

    private func drainDeliveredFrames(client: PhantomClient, gen: UInt64) {
        let frames = client.takeFrames()
        guard !frames.isEmpty, isGenerationActive(gen) else { return }
        var readySamples: [CMSampleBuffer] = []
        var updatedSize: (width: Int, height: Int)?

        sampleLock.lock()
        for f in frames {
            bytesInWindow += f.data.count
            framesInWindow += 1
            if let sb = sampleBuilder.sampleBuffer(from: f.data, codec: f.codec) {
                readySamples.append(sb)
                let w = sampleBuilder.videoWidth
                let h = sampleBuilder.videoHeight
                if w > 0, h > 0, (w != lastReportedVideoSize.width || h != lastReportedVideoSize.height) {
                    lastReportedVideoSize = (w, h)
                    updatedSize = (w, h)
                }
            }
        }
        sampleLock.unlock()

        if let size = updatedSize {
            DispatchQueue.main.async { self.videoSize = size }
        }
        for sb in readySamples {
            onSample?(sb)
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
        let videoW = sampleBuilder.videoWidth
        let videoH = sampleBuilder.videoHeight
        windowStart = nowMedia
        framesInWindow = 0
        bytesInWindow = 0
        sampleLock.unlock()

        let negotiated = client.negotiated()
        let counters = client.counters(nowUs: nowUs)
        let hostCounters = withLock { hostLoopback?.hostCounters() }

        var snap = RemoteSessionMetrics()
        snap.connected = negotiated != nil && !client.isClosed()
        snap.fps = fps
        snap.bitrateMbps = mbps
        snap.rttMs = counters.rttUs.map { Double($0) / 1000.0 }
        snap.queueDelayMs = counters.queueDelayUs.map { Double($0) / 1000.0 }
        snap.framesDelivered = counters.framesOk
        snap.framesLost = counters.framesLost
        snap.videoWidth = videoW
        snap.videoHeight = videoH
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

        guard isGenerationActive(gen) else { return }
        DispatchQueue.main.async {
            self.metrics = snap
            if snap.connected {
                self.statusText = "Connesso (\(snap.codecName))"
            }
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
                    // Svuota eventi di tastiera e tocco in loopback così i contatori restano coerenti
                    while phantomHost.takeTouch() != nil {}
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

        var frameIndex = 0
        let frameIntervalNs = UInt64(1_000_000_000 / max(1, fps))
        let duration = CMTime(value: 1, timescale: CMTimeScale(fps))

        while !isStopped && !conn.isClosed() && !host.isClosed() {
            let frameStart = DispatchTime.now().uptimeNanoseconds
            var pbOut: CVPixelBuffer?
            if CVPixelBufferPoolCreatePixelBuffer(nil, bufferPool, &pbOut) == kCVReturnSuccess, let pb = pbOut {
                fillSyntheticFrame(pb, index: frameIndex)
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
                ) { status, _, sample in
                    guard status == noErr, let sb = sample,
                          let annex = Self.annexB(from: sb) else { return }
                    let key = Self.isKeyFrame(sb)
                    for d in host.sendFrame(key: key, captureTsUs: captureUs, data: annex) {
                        try? conn.sendDatagram(datagram: d)
                    }
                }
                frameIndex &+= 1
            }

            let elapsedNs = DispatchTime.now().uptimeNanoseconds - frameStart
            if elapsedNs < frameIntervalNs {
                usleep(UInt32((frameIntervalNs - elapsedNs) / 1_000))
            }
        }
    }

    private func fillSyntheticFrame(_ pb: CVPixelBuffer, index: Int) {
        CVPixelBufferLockBaseAddress(pb, [])
        defer { CVPixelBufferUnlockBaseAddress(pb, []) }
        if let yBase = CVPixelBufferGetBaseAddressOfPlane(pb, 0) {
            let h = CVPixelBufferGetHeightOfPlane(pb, 0)
            let w = CVPixelBufferGetWidthOfPlane(pb, 0)
            let stride = CVPixelBufferGetBytesPerRowOfPlane(pb, 0)
            let barTop = (index * 16) % max(1, h - 80)
            for row in 0..<h {
                let luma: Int32 = (row >= barTop && row < barTop + 80) ? 220 : Int32(32 + ((row + index * 2) % 96))
                memset(yBase + row * stride, luma, w)
            }
        }
        if let uvBase = CVPixelBufferGetBaseAddressOfPlane(pb, 1) {
            let h = CVPixelBufferGetHeightOfPlane(pb, 1)
            let stride = CVPixelBufferGetBytesPerRowOfPlane(pb, 1)
            memset(uvBase, Int32(96 + (index % 64)), h * stride)
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
