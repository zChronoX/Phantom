import Foundation
import Network
import CoreMedia
import VideoToolbox
import QuartzCore

/// Riceve il flusso del server di scrcpy (via il ponte della sonda, TCP 47803), lo interpreta con
/// `ScrcpyStreamParser` e lo manda al decoder hardware (H.264 o H.265).
///
/// A differenza del flusso di base di `screenrecord`, il server di scrcpy è pensato per la bassa
/// latenza: encoder a bassa latenza, frame rate scelto e nessun buffer lato telefono.
final class ScrcpyClient: VideoSource {
    var onSample: ((CMSampleBuffer) -> Void)?
    var onStats: ((VideoStats) -> Void)?
    var onStatus: ((String) -> Void)?

    private let queue = DispatchQueue(label: "app.phantom.f0.scrcpy", qos: .userInteractive)
    private var connection: NWConnection?
    private var parser = ScrcpyStreamParser()
    private var codec: ScrcpyCodec?
    private var formatDesc: CMVideoFormatDescription?

    private var stats = VideoStats()
    private var windowStart = CACurrentMediaTime()
    private var bytesInWindow = 0
    private var framesInWindow = 0
    private var lastFrameTime = 0.0
    private var gaps: [Double] = []

    // Ritardo extra: (tempo di arrivo − PTS del frame) rispetto al migliore recente.
    private var firstArrival: Double?
    private var firstPts: UInt64?
    private var delays: [Double] = []

    // Tutto lo stato di questa classe si tocca SOLO dalla coda `queue`.
    private var generation = 0

    /// Ogni connessione ha una "generazione": i callback di una connessione vecchia vengono ignorati e la
    /// connessione vecchia viene chiusa, mai abbandonata. Una connessione non letta riempie i buffer del POCO,
    /// il server scrcpy si blocca e può bloccare lo schermo dell'intero telefono.
    private func report(_ text: String) {
        let cb = onStatus
        DispatchQueue.main.async { cb?(text) }
    }

    func start(host: String) {
        queue.async { self.startLocked(host: host) }
    }

    func stop() {
        queue.async {
            self.generation &+= 1
            self.teardown()
            self.stats.connected = false
            self.publish()
        }
    }

    private func teardown() {
        connection?.stateUpdateHandler = nil
        connection?.cancel()
        connection = nil
    }

    private func startLocked(host: String) {
        teardown()
        let h = host.trimmingCharacters(in: .whitespaces)
        guard !h.isEmpty, let port = NWEndpoint.Port(rawValue: 47803) else {
            report("Indirizzo IP del POCO mancante")
            return
        }
        generation &+= 1
        let gen = generation
        parser = ScrcpyStreamParser()
        codec = nil
        formatDesc = nil
        stats = VideoStats()
        windowStart = CACurrentMediaTime()
        bytesInWindow = 0; framesInWindow = 0
        lastFrameTime = 0; gaps.removeAll()
        firstArrival = nil; firstPts = nil; delays.removeAll()

        let params = NWParameters.tcp
        if let tcp = params.defaultProtocolStack.transportProtocol as? NWProtocolTCP.Options {
            tcp.noDelay = true
        }
        let conn = NWConnection(host: NWEndpoint.Host(h), port: port, using: params)
        connection = conn
        report("Connessione a \(h):47803…")
        conn.stateUpdateHandler = { [weak self] state in
            guard let self = self, self.generation == gen else { return }
            switch state {
            case .ready:
                self.stats.connected = true
                self.publish()
                self.report("Connesso. Avvio del server scrcpy sul POCO (circa 1 secondo)…")
                self.receive(on: conn, generation: gen)
            case .waiting(let error):
                self.report("In attesa della rete: \(error.localizedDescription)")
            case .failed(let error):
                self.report("Connessione fallita: \(error.localizedDescription)")
                self.stats.connected = false
                self.publish()
            default:
                break
            }
        }
        conn.start(queue: queue)
    }

    // MARK: - Rete

    private func receive(on conn: NWConnection, generation gen: Int) {
        conn.receive(minimumIncompleteLength: 1, maximumLength: 262_144) { [weak self] data, _, isComplete, error in
            guard let self = self, self.generation == gen else { return }
            if let data = data, !data.isEmpty {
                self.bytesInWindow += data.count
                for event in self.parser.feed(data) { self.handle(event) }
            }
            if let error = error {
                self.report("Flusso interrotto: \(error.localizedDescription)")
                return
            }
            if isComplete {
                self.report("Il POCO ha chiuso il flusso (sessione terminata dalla sonda)")
                self.stats.connected = false
                self.publish()
                return
            }
            // Il parser può aver chiuso la connessione per un flusso non valido (vedi handle): niente più letture.
            if self.generation == gen { self.receive(on: conn, generation: gen) }
        }
    }

    // MARK: - Eventi del flusso

    private func handle(_ event: ScrcpyEvent) {
        switch event {
        case .deviceName(let name):
            report("Connesso a \(name). Attendo il primo frame…")
        case .codec(let id):
            guard let c = ScrcpyCodec(rawValue: id), c == .h264 || c == .h265 else {
                report("Codec non supportato dall'app (id \(String(id, radix: 16))): usa H.264 o H.265")
                generation &+= 1
                teardown()
                return
            }
            codec = c
            stats.codec = c == .h265 ? "H.265" : "H.264"
        case .session(let w, let h):
            stats.width = w
            stats.height = h
        case .config(let data):
            buildFormat(from: data)
        case .frame(let data, let pts, _):
            emit(data, ptsUs: pts)
        case .failure(let message):
            report("Flusso non valido: \(message)")
            generation &+= 1
            teardown()
        }
    }

    private func buildFormat(from config: Data) {
        guard let codec = codec else { return }
        let ps = AnnexB.parameterSets(from: AnnexB.splitNALs(config), codec: codec)
        var fmt: CMVideoFormatDescription?
        var status: OSStatus = -1

        if codec == .h264, let sps = ps.sps, let pps = ps.pps {
            status = withParameterSets([sps, pps]) { pointers, sizes in
                CMVideoFormatDescriptionCreateFromH264ParameterSets(
                    allocator: kCFAllocatorDefault, parameterSetCount: 2,
                    parameterSetPointers: pointers, parameterSetSizes: sizes,
                    nalUnitHeaderLength: 4, formatDescriptionOut: &fmt)
            }
        } else if codec == .h265, let vps = ps.vps, let sps = ps.sps, let pps = ps.pps {
            status = withParameterSets([vps, sps, pps]) { pointers, sizes in
                CMVideoFormatDescriptionCreateFromHEVCParameterSets(
                    allocator: kCFAllocatorDefault, parameterSetCount: 3,
                    parameterSetPointers: pointers, parameterSetSizes: sizes,
                    nalUnitHeaderLength: 4, extensions: nil, formatDescriptionOut: &fmt)
            }
        }
        if status == noErr, let fmt = fmt {
            formatDesc = fmt
            let dims = CMVideoFormatDescriptionGetDimensions(fmt)
            stats.width = Int(dims.width)
            stats.height = Int(dims.height)
        } else {
            self.report("Parametri del video non validi (codice \(status))")
        }
    }

    /// Passa i parameter set a VideoToolbox come array di puntatori validi per tutta la chiamata.
    private func withParameterSets(_ sets: [Data], _ body: (UnsafePointer<UnsafePointer<UInt8>>, UnsafePointer<Int>) -> OSStatus) -> OSStatus {
        let copies = sets.map { [UInt8]($0) }
        let sizes = copies.map { $0.count }
        func nest(_ i: Int, _ pointers: [UnsafePointer<UInt8>]) -> OSStatus {
            if i == copies.count {
                return pointers.withUnsafeBufferPointer { p in
                    sizes.withUnsafeBufferPointer { s in body(p.baseAddress!, s.baseAddress!) }
                }
            }
            return copies[i].withUnsafeBufferPointer { buf in nest(i + 1, pointers + [buf.baseAddress!]) }
        }
        return nest(0, [])
    }

    private func emit(_ data: Data, ptsUs: UInt64) {
        guard let codec = codec, let fmt = formatDesc else { return } // si aspetta la configurazione
        let nals = AnnexB.splitNALs(data).filter { !AnnexB.isParameterSet($0, codec: codec) }
        guard !nals.isEmpty else { return }
        let avcc = AnnexB.toLengthPrefixed(nals)

        var blockBuffer: CMBlockBuffer?
        var status = CMBlockBufferCreateWithMemoryBlock(
            allocator: kCFAllocatorDefault, memoryBlock: nil, blockLength: avcc.count,
            blockAllocator: kCFAllocatorDefault, customBlockSource: nil,
            offsetToData: 0, dataLength: avcc.count, flags: 0, blockBufferOut: &blockBuffer)
        guard status == kCMBlockBufferNoErr, let bb = blockBuffer else { return }
        status = avcc.withUnsafeBytes {
            CMBlockBufferReplaceDataBytes(with: $0.baseAddress!, blockBuffer: bb, offsetIntoDestination: 0, dataLength: avcc.count)
        }
        guard status == kCMBlockBufferNoErr else { return }

        var sample: CMSampleBuffer?
        var sizes = [avcc.count]
        status = CMSampleBufferCreateReady(
            allocator: kCFAllocatorDefault, dataBuffer: bb, formatDescription: fmt,
            sampleCount: 1, sampleTimingEntryCount: 0, sampleTimingArray: nil,
            sampleSizeEntryCount: 1, sampleSizeArray: &sizes, sampleBufferOut: &sample)
        guard status == noErr, let sb = sample else { return }

        if let attachments = CMSampleBufferGetSampleAttachmentsArray(sb, createIfNecessary: true) {
            let dict = unsafeBitCast(CFArrayGetValueAtIndex(attachments, 0), to: CFMutableDictionary.self)
            CFDictionarySetValue(dict,
                Unmanaged.passUnretained(kCMSampleAttachmentKey_DisplayImmediately).toOpaque(),
                Unmanaged.passUnretained(kCFBooleanTrue).toOpaque())
        }

        onSample?(sb)
        tickMetrics(ptsUs: ptsUs)
    }

    // MARK: - Metriche

    private func tickMetrics(ptsUs: UInt64) {
        let now = CACurrentMediaTime()
        if lastFrameTime > 0 { gaps.append((now - lastFrameTime) * 1000) }
        lastFrameTime = now
        framesInWindow += 1

        if firstArrival == nil { firstArrival = now; firstPts = ptsUs }
        if let t0 = firstArrival, let p0 = firstPts {
            let d = (now - t0) * 1000 - (Double(ptsUs) - Double(p0)) / 1000
            delays.append(d)
            if delays.count > 300 { delays.removeFirst(delays.count - 300) }
        }

        let elapsed = now - windowStart
        if elapsed >= 0.5 {
            stats.fps = Double(framesInWindow) / elapsed
            stats.kbps = Double(bytesInWindow) * 8 / 1000 / elapsed
            stats.frames += framesInWindow
            if !gaps.isEmpty {
                stats.meanGapMs = gaps.reduce(0, +) / Double(gaps.count)
                stats.maxGapMs = gaps.max() ?? 0
            }
            if let best = delays.min(), delays.count >= 30 {
                let excess = delays.map { $0 - best }.sorted()
                stats.excessP50Ms = excess[excess.count / 2]
                stats.excessP95Ms = excess[min(excess.count - 1, excess.count * 95 / 100)]
            }
            windowStart = now
            framesInWindow = 0
            bytesInWindow = 0
            gaps.removeAll(keepingCapacity: true)
            publish()
        }
    }

    private func publish() {
        let s = stats
        DispatchQueue.main.async { self.onStats?(s) }
    }
}
