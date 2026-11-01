import Foundation
import Network
import CoreMedia
import VideoToolbox
import QuartzCore

/// Riceve il flusso H.264 (Annex-B) dalla sonda sul POCO (TCP 47802), lo spezza in
/// unità NAL e ne costruisce dei CMSampleBuffer pronti per la decodifica hardware.
///
/// È la linea di base della fase 0: il video arriva da `screenrecord`, non ancora
/// dalla pipeline ottimizzata di Phantom. Serve a vedere lo schermo dal vivo sull'iPhone
/// e a misurare frame rate, banda e regolarità dei frame col vero percorso di decodifica.
final class VideoClient {
    struct Stats {
        var connected = false
        var fps = 0.0
        var kbps = 0.0
        var frames = 0
        var width = 0
        var height = 0
        var meanGapMs = 0.0
        var maxGapMs = 0.0
    }

    var onSample: ((CMSampleBuffer) -> Void)?
    var onStats: ((Stats) -> Void)?
    var onStatus: ((String) -> Void)?

    private let queue = DispatchQueue(label: "app.phantom.f0.video", qos: .userInteractive)
    private var connection: NWConnection?
    private var buffer = Data()

    private var sps: Data?
    private var pps: Data?
    private var formatDesc: CMVideoFormatDescription?

    private var stats = Stats()
    private var windowStart = CACurrentMediaTime()
    private var bytesInWindow = 0
    private var framesInWindow = 0
    private var lastFrameTime = 0.0
    private var gaps: [Double] = []

    // MARK: - Ciclo di vita

    func start(host: String, port: UInt16 = 47802) {
        let h = host.trimmingCharacters(in: .whitespaces)
        guard !h.isEmpty, let p = NWEndpoint.Port(rawValue: port) else {
            onStatus?("Indirizzo IP del POCO mancante")
            return
        }
        let params = NWParameters.tcp
        if let tcp = params.defaultProtocolStack.transportProtocol as? NWProtocolTCP.Options {
            tcp.noDelay = true
        }
        let conn = NWConnection(host: NWEndpoint.Host(h), port: p, using: params)
        connection = conn
        resetState()
        onStatus?("Connessione a \(h):\(port)…")
        conn.stateUpdateHandler = { [weak self] state in
            guard let self = self else { return }
            switch state {
            case .ready:
                self.stats.connected = true
                self.publish()
                self.onStatus?("Connesso. In attesa dei frame… (accendi il video nella WebUI del POCO)")
                self.receive()
            case .waiting(let error):
                self.onStatus?("In attesa della rete: \(error.localizedDescription)")
            case .failed(let error):
                self.onStatus?("Connessione fallita: \(error.localizedDescription)")
                self.stats.connected = false
                self.publish()
            case .cancelled:
                self.stats.connected = false
            default:
                break
            }
        }
        conn.start(queue: queue)
    }

    func stop() {
        connection?.cancel()
        connection = nil
        stats.connected = false
        publish()
    }

    private func resetState() {
        queue.async {
            self.buffer.removeAll(keepingCapacity: true)
            self.sps = nil; self.pps = nil; self.formatDesc = nil
            self.stats = Stats()
            self.windowStart = CACurrentMediaTime()
            self.bytesInWindow = 0; self.framesInWindow = 0
            self.lastFrameTime = 0; self.gaps.removeAll()
        }
    }

    // MARK: - Rete

    private func receive() {
        connection?.receive(minimumIncompleteLength: 1, maximumLength: 262144) { [weak self] data, _, isComplete, error in
            guard let self = self else { return }
            if let data = data, !data.isEmpty {
                self.bytesInWindow += data.count
                self.buffer.append(data)
                self.parse()
            }
            if let error = error {
                self.onStatus?("Flusso interrotto: \(error.localizedDescription)")
                return
            }
            if isComplete {
                self.onStatus?("Flusso chiuso dal telefono")
                return
            }
            self.receive()
        }
    }

    // MARK: - Parsing Annex-B

    /// Trova il prossimo start code (00 00 01 oppure 00 00 00 01) a partire da `from`.
    private func startCode(from: Int) -> (pos: Int, len: Int)? {
        let n = buffer.count
        var i = max(0, from)
        let base = buffer.startIndex
        while i + 3 <= n {
            if buffer[base + i] == 0 && buffer[base + i + 1] == 0 {
                if buffer[base + i + 2] == 1 { return (i, 3) }
                if i + 4 <= n && buffer[base + i + 2] == 0 && buffer[base + i + 3] == 1 { return (i, 4) }
            }
            i += 1
        }
        return nil
    }

    /// Estrae le NAL complete dal buffer: ogni NAL è chiusa quando arriva lo start code successivo.
    private func parse() {
        while true {
            guard let s0 = startCode(from: 0) else {
                if buffer.count > 3 { buffer.removeSubrange(0..<(buffer.count - 3)) }
                return
            }
            if s0.pos > 0 { buffer.removeSubrange(0..<s0.pos) } // il buffer ora inizia con uno start code
            guard let lead = startCode(from: 0) else { return }
            guard let s1 = startCode(from: lead.len) else { return } // NAL non ancora completa
            let base = buffer.startIndex
            let nal = buffer.subdata(in: (base + lead.len)..<(base + s1.pos))
            buffer.removeSubrange(0..<s1.pos)
            if !nal.isEmpty { handleNAL(nal) }
        }
    }

    private func handleNAL(_ nal: Data) {
        let type = nal[nal.startIndex] & 0x1F
        switch type {
        case 7: sps = nal; rebuildFormat()        // SPS
        case 8: pps = nal; rebuildFormat()        // PPS
        case 1, 5: emitFrame(nal)                 // slice non-IDR / IDR
        default: break                            // SEI, AUD, ecc.: ignorati
        }
    }

    private func rebuildFormat() {
        guard let sps = sps, let pps = pps else { return }
        var fmt: CMVideoFormatDescription?
        let status = sps.withUnsafeBytes { (spsRaw: UnsafeRawBufferPointer) -> OSStatus in
            pps.withUnsafeBytes { (ppsRaw: UnsafeRawBufferPointer) -> OSStatus in
                let pointers = [spsRaw.bindMemory(to: UInt8.self).baseAddress!,
                                ppsRaw.bindMemory(to: UInt8.self).baseAddress!]
                let sizes = [sps.count, pps.count]
                return pointers.withUnsafeBufferPointer { pp in
                    sizes.withUnsafeBufferPointer { ss in
                        CMVideoFormatDescriptionCreateFromH264ParameterSets(
                            allocator: kCFAllocatorDefault,
                            parameterSetCount: 2,
                            parameterSetPointers: pp.baseAddress!,
                            parameterSetSizes: ss.baseAddress!,
                            NALUnitHeaderLength: 4,
                            formatDescriptionOut: &fmt)
                    }
                }
            }
        }
        if status == noErr, let fmt = fmt {
            formatDesc = fmt
            let dims = CMVideoFormatDescriptionGetDimensions(fmt)
            stats.width = Int(dims.width)
            stats.height = Int(dims.height)
        }
    }

    /// Converte la NAL in formato AVCC (prefisso di lunghezza a 4 byte) e la impacchetta
    /// in un CMSampleBuffer con "mostra subito", per la latenza minima.
    private func emitFrame(_ nal: Data) {
        guard let fmt = formatDesc else { return } // niente SPS/PPS ancora: aspetta l'IDR iniziale
        var avcc = Data(capacity: 4 + nal.count)
        var len = UInt32(nal.count).bigEndian
        withUnsafeBytes(of: &len) { avcc.append(contentsOf: $0) }
        avcc.append(nal)

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
        tickMetrics()
    }

    // MARK: - Metriche

    private func tickMetrics() {
        let now = CACurrentMediaTime()
        if lastFrameTime > 0 { gaps.append((now - lastFrameTime) * 1000) }
        lastFrameTime = now
        framesInWindow += 1

        let elapsed = now - windowStart
        if elapsed >= 0.5 {
            stats.fps = Double(framesInWindow) / elapsed
            stats.kbps = Double(bytesInWindow) * 8 / 1000 / elapsed
            stats.frames += framesInWindow
            if !gaps.isEmpty {
                let mean = gaps.reduce(0, +) / Double(gaps.count)
                stats.meanGapMs = mean
                stats.maxGapMs = gaps.max() ?? 0
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
