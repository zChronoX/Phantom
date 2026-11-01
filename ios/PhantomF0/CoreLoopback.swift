import Foundation
import UIKit
import AVFoundation
import VideoToolbox
import CoreMedia
import CoreVideo

// Prova «tutto sul telefono» del core Rust di Phantom.
//
// L'iPhone fa da solo l'intero percorso del video, senza il POCO e senza rete:
//
//   sorgente sintetica → encoder HEVC hardware → PhantomHost (core Rust: frammenti, parità, ritrasmissioni, controllo del
//   bitrate) → perdite simulate → PhantomClient (core Rust: riassemblaggio, sincronia degli orologi) → decoder e schermo
//
// Serve a tre cose: provare il core Rust sul telefono vero (i binding UniFFI, la libreria statica, il costo in CPU a
// 120 fps), misurare la latenza del percorso con perdite e vedere sullo schermo il risultato; e riusare lo stesso
// codice quando arriverà il server vero sul POCO. Il tocco fa il giro inverso: un dito finto disegna cerchi, il client
// ne manda lo stato a 240 Hz e l'host lo riceve.

private func nowNs() -> UInt64 { DispatchTime.now().uptimeNanoseconds }
private func nowUs() -> UInt64 { nowNs() / 1000 }

/// Quanto l'orologio dell'host è avanti rispetto a quello del client in questa prova: la sincronia deve scoprirlo da sola.
private let hostClockOffsetUs: UInt64 = 3_000_000
private func hostUs() -> UInt64 { nowUs() + hostClockOffsetUs }

struct LoopbackSettings: Equatable {
    var fps = 120
    var lossPercent = 2.0
    var seconds = 20
    var width = 1080
    var height = 2400
}

private final class StopFlag {
    private let lock = NSLock()
    private var value = false
    var isSet: Bool {
        lock.lock(); defer { lock.unlock() }
        return value
    }
    func set() {
        lock.lock(); value = true; lock.unlock()
    }
}

/// Misure raccolte da più thread; si modificano solo con `add`.
private final class LoopbackMetrics {
    private let lock = NSLock()
    var sourceFrames = 0
    var encodedFrames = 0
    var encodeFailures = 0
    var keyFrames = 0
    var delivered = 0
    var displayed = 0
    var datagramsSent = 0
    var datagramsDropped = 0
    var touchSent = 0
    var touchReceived = 0
    var encodedBytes = 0
    var coreLatencyMs: [Double] = []
    var encodeMs: [Double] = []
    var sendFrameUs: [Double] = []
    var onDatagramUs: [Double] = []
    var touchAgeMs: [Double] = []
    var bitrateTrace: [Double] = []

    func add(_ body: (LoopbackMetrics) -> Void) {
        lock.lock(); defer { lock.unlock() }
        body(self)
    }

    func read<T>(_ body: (LoopbackMetrics) -> T) -> T {
        lock.lock(); defer { lock.unlock() }
        return body(self)
    }
}

final class CoreLoopback: ObservableObject {
    @Published private(set) var running = false
    @Published private(set) var progress = ""
    @Published private(set) var report = ""

    /// Chi mostra il video: riceve i frame pronti per il livello di visualizzazione.
    var onSample: ((CMSampleBuffer) -> Void)?

    private var stopFlag = StopFlag()

    func start(_ settings: LoopbackSettings) {
        guard !running else { return }
        running = true
        report = ""
        progress = "Preparazione…"
        UIApplication.shared.isIdleTimerDisabled = true
        let flag = StopFlag()
        stopFlag = flag
        DispatchQueue.global(qos: .userInteractive).async {
            let run = LoopbackRun(
                settings: settings, stop: flag,
                onSample: { [weak self] sb in self?.onSample?(sb) },
                onProgress: { [weak self] text in DispatchQueue.main.async { self?.progress = text } })
            let text = run.execute()
            DispatchQueue.main.async {
                self.report = text
                self.progress = flag.isSet ? "Fermata" : "Finito"
                self.running = false
                UIApplication.shared.isIdleTimerDisabled = false
            }
        }
    }

    func stop() { stopFlag.set() }
}

private final class LoopbackRun {
    let s: LoopbackSettings
    let stop: StopFlag
    let onSample: (CMSampleBuffer) -> Void
    let onProgress: (String) -> Void

    let settings: SessionSettings
    let host: PhantomHost
    let client: PhantomClient
    let lossP: Double
    let m = LoopbackMetrics()

    let netQueue = DispatchQueue(label: "app.phantom.f0.loopback.net", qos: .userInteractive)
    var tickTimer: DispatchSourceTimer?

    // Lato client: toccati solo dalla coda `netQueue`.
    var formatDesc: CMVideoFormatDescription?
    var lastParameterSets: [Data] = []

    init(settings loopback: LoopbackSettings, stop: StopFlag, onSample: @escaping (CMSampleBuffer) -> Void,
         onProgress: @escaping (String) -> Void) {
        s = loopback
        self.stop = stop
        self.onSample = onSample
        self.onProgress = onProgress
        var cfg = defaultSessionSettings()
        cfg.codec = .h265
        settings = cfg
        host = PhantomHost(settings: cfg)
        client = PhantomClient(settings: cfg)
        lossP = max(0, min(0.5, loopback.lossPercent / 100))
    }

    // MARK: Esecuzione

    func execute() -> String {
        guard let encoder = makeEncoder() else { return "Impossibile creare l'encoder HEVC dell'iPhone." }
        defer { VTCompressionSessionInvalidate(encoder) }
        var pool = VTCompressionSessionGetPixelBufferPool(encoder)
        if pool == nil {
            let attrs: [CFString: Any] = [kCVPixelBufferPixelFormatTypeKey: kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange,
                                          kCVPixelBufferWidthKey: s.width, kCVPixelBufferHeightKey: s.height,
                                          kCVPixelBufferIOSurfacePropertiesKey: [String: Any]()]
            CVPixelBufferPoolCreate(nil, nil, attrs as CFDictionary, &pool)
        }
        guard let bufferPool = pool else { return "Nessun pool di buffer per l'encoder." }

        let thermalStart = DecoderBench.thermal()
        let source = Source(width: s.width, height: s.height)
        var appliedBitrate = Int(host.targetBitrateBps())
        startTimer()

        let cpu0 = cpuSeconds()
        let wall0 = nowNs()
        let total = s.fps * s.seconds
        let start = nowNs() + 100_000_000
        let frameDuration = CMTime(value: 1, timescale: CMTimeScale(s.fps))
        for i in 0..<total {
            if stop.isSet { break }
            sleepUntil(start + UInt64(i) * 1_000_000_000 / UInt64(s.fps))

            // Ogni mezzo secondo l'encoder segue il bitrate che il controllo di Phantom ha deciso.
            if i % (s.fps / 2) == 0 {
                let target = Int(host.targetBitrateBps())
                m.add { $0.bitrateTrace.append(Double(target)) }
                if abs(target - appliedBitrate) * 100 > appliedBitrate * 8 {
                    VTSessionSetProperty(encoder, key: kVTCompressionPropertyKey_AverageBitRate, value: NSNumber(value: target))
                    appliedBitrate = target
                }
            }

            var pbOut: CVPixelBuffer?
            guard CVPixelBufferPoolCreatePixelBuffer(nil, bufferPool, &pbOut) == kCVReturnSuccess, let pb = pbOut else {
                m.add { $0.encodeFailures += 1 }
                continue
            }
            source.fill(pb, frame: i)
            m.add { $0.sourceFrames += 1 }

            let submitNs = nowNs()
            let captureHostUs = submitNs / 1000 + hostClockOffsetUs
            // L'encoder produce un keyframe quando l'host lo chiede (richiesta del client dopo una perdita, o inizio).
            let forceKey = i == 0 || host.wantsKeyframe(nowUs: captureHostUs)
            let props: CFDictionary? = forceKey ? [kVTEncodeFrameOptionKey_ForceKeyFrame: true] as CFDictionary : nil
            let pts = CMTime(value: CMTimeValue(i), timescale: CMTimeScale(s.fps))
            let r = VTCompressionSessionEncodeFrame(encoder, imageBuffer: pb, presentationTimeStamp: pts, duration: frameDuration,
                                                    frameProperties: props, infoFlagsOut: nil) { status, _, sample in
                self.encoded(status: status, sample: sample, captureHostUs: captureHostUs, submitNs: submitNs)
            }
            if r != noErr { m.add { $0.encodeFailures += 1 } }

            if i % s.fps == 0 { onProgress(liveText(second: i / s.fps, of: s.seconds)) }
        }
        VTCompressionSessionCompleteFrames(encoder, untilPresentationTimeStamp: CMTime.invalid)
        usleep(500_000) // lascia arrivare ritrasmissioni e ultimi frame
        let wallSeconds = Double(nowNs() - wall0) / 1e9
        let cpuSecondsUsed = cpuSeconds() - cpu0

        tickTimer?.cancel()
        netQueue.sync {}
        return makeReport(wallSeconds: wallSeconds, cpuSeconds: cpuSecondsUsed, thermalStart: thermalStart)
    }

    private func liveText(second: Int, of seconds: Int) -> String {
        let (delivered, sent, touches) = m.read { ($0.delivered, $0.sourceFrames, $0.touchReceived) }
        let mbps = Double(host.targetBitrateBps()) / 1e6
        return "\(second) s su \(seconds) · frame consegnati \(delivered) su \(sent) · bitrate richiesto \(String(format: "%.1f", mbps)) Mbps · tocchi all'host \(touches)"
    }

    // MARK: Encoder

    private func makeEncoder() -> VTCompressionSession? {
        var enc: VTCompressionSession?
        let lowLatency: [CFString: Any] = [kVTVideoEncoderSpecification_EnableLowLatencyRateControl: true]
        var st = VTCompressionSessionCreate(allocator: nil, width: Int32(s.width), height: Int32(s.height),
                                            codecType: kCMVideoCodecType_HEVC, encoderSpecification: lowLatency as CFDictionary,
                                            imageBufferAttributes: nil, compressedDataAllocator: nil, outputCallback: nil,
                                            refcon: nil, compressionSessionOut: &enc)
        if st != noErr || enc == nil {
            st = VTCompressionSessionCreate(allocator: nil, width: Int32(s.width), height: Int32(s.height),
                                            codecType: kCMVideoCodecType_HEVC, encoderSpecification: nil,
                                            imageBufferAttributes: nil, compressedDataAllocator: nil, outputCallback: nil,
                                            refcon: nil, compressionSessionOut: &enc)
        }
        guard st == noErr, let session = enc else { return nil }
        VTSessionSetProperty(session, key: kVTCompressionPropertyKey_RealTime, value: kCFBooleanTrue)
        VTSessionSetProperty(session, key: kVTCompressionPropertyKey_AllowFrameReordering, value: kCFBooleanFalse)
        VTSessionSetProperty(session, key: kVTCompressionPropertyKey_ExpectedFrameRate, value: NSNumber(value: s.fps))
        VTSessionSetProperty(session, key: kVTCompressionPropertyKey_AverageBitRate, value: NSNumber(value: host.targetBitrateBps()))
        // Un keyframe periodico ogni 10 s come rete di sicurezza; gli altri li chiede l'host quando serve.
        VTSessionSetProperty(session, key: kVTCompressionPropertyKey_MaxKeyFrameInterval, value: NSNumber(value: s.fps * 10))
        VTSessionSetProperty(session, key: kVTCompressionPropertyKey_MaxFrameDelayCount, value: NSNumber(value: 0))
        VTCompressionSessionPrepareToEncodeFrames(session)
        return session
    }

    /// Un frame uscito dall'encoder (thread di VideoToolbox): in Annex-B, come arriverebbe da MediaCodec, e all'host.
    private func encoded(status: OSStatus, sample: CMSampleBuffer?, captureHostUs: UInt64, submitNs: UInt64) {
        let tEnc = nowNs()
        guard status == noErr, let sb = sample, let annex = LoopbackRun.annexB(from: sb) else {
            m.add { $0.encodeFailures += 1 }
            return
        }
        let key = LoopbackRun.isKeyFrame(sb)
        m.add {
            $0.encodedFrames += 1
            $0.encodedBytes += annex.count
            $0.encodeMs.append(Double(tEnc - submitNs) / 1e6)
            if key { $0.keyFrames += 1 }
        }
        let t0 = nowNs()
        let datagrams = host.sendFrame(key: key, captureTsUs: captureHostUs, data: annex)
        let elapsed = nowNs() - t0
        m.add { $0.sendFrameUs.append(Double(elapsed) / 1000) }
        for d in datagrams { sendDownlink(d) }
    }

    static func isKeyFrame(_ sb: CMSampleBuffer) -> Bool {
        let attachments = CMSampleBufferGetSampleAttachmentsArray(sb, createIfNecessary: false) as? [[CFString: Any]]
        let notSync = attachments?.first?[kCMSampleAttachmentKey_NotSync] as? Bool ?? false
        return !notSync
    }

    /// Da campione di VideoToolbox (NAL con prefisso di lunghezza, parametri nel formato) ad Annex-B con i parametri
    /// davanti ai keyframe.
    static func annexB(from sb: CMSampleBuffer) -> Data? {
        guard let fd = CMSampleBufferGetFormatDescription(sb), let block = CMSampleBufferGetDataBuffer(sb) else { return nil }
        var count = 0
        var nalLength: Int32 = 4
        let probe = CMVideoFormatDescriptionGetHEVCParameterSetAtIndex(
            fd, parameterSetIndex: 0, parameterSetPointerOut: nil, parameterSetSizeOut: nil,
            parameterSetCountOut: &count, nalUnitHeaderLengthOut: &nalLength)
        guard probe == noErr else { return nil }

        let size = CMBlockBufferGetDataLength(block)
        var bytes = [UInt8](repeating: 0, count: size)
        guard CMBlockBufferCopyDataBytes(block, atOffset: 0, dataLength: size, destination: &bytes) == kCMBlockBufferNoErr else { return nil }

        var out = Data()
        if isKeyFrame(sb) {
            var sets: [Data] = []
            for i in 0..<count {
                var pointer: UnsafePointer<UInt8>?
                var setSize = 0
                let st = CMVideoFormatDescriptionGetHEVCParameterSetAtIndex(
                    fd, parameterSetIndex: i, parameterSetPointerOut: &pointer, parameterSetSizeOut: &setSize,
                    parameterSetCountOut: nil, nalUnitHeaderLengthOut: nil)
                if st == noErr, let p = pointer { sets.append(Data(bytes: p, count: setSize)) }
            }
            out.append(AnnexBConvert.parameterSets(sets))
        }
        out.append(AnnexBConvert.fromLengthPrefixed(Data(bytes), lengthSize: Int(nalLength)))
        return out.isEmpty ? nil : out
    }

    // MARK: Rete simulata

    /// Dall'host al client, con perdita casuale.
    private func sendDownlink(_ d: Data) {
        m.add { $0.datagramsSent += 1 }
        if lossP > 0 && Double.random(in: 0..<1) < lossP {
            m.add { $0.datagramsDropped += 1 }
            return
        }
        netQueue.async { self.receiveAtClient(d) }
    }

    /// Dal client all'host, senza perdite (sono pochi messaggi: richieste, ping, tocco).
    private func sendUplink(_ d: Data) {
        for reply in host.onDatagram(nowUs: hostUs(), data: d) { sendDownlink(reply) }
    }

    private func receiveAtClient(_ d: Data) {
        let t0 = nowNs()
        let replies = client.onDatagram(nowUs: nowUs(), data: d)
        let elapsed = nowNs() - t0
        m.add { $0.onDatagramUs.append(Double(elapsed) / 1000) }
        for r in replies { sendUplink(r) }
    }

    private func startTimer() {
        let timer = DispatchSource.makeTimerSource(flags: .strict, queue: netQueue)
        timer.schedule(deadline: .now(), repeating: .milliseconds(1), leeway: .microseconds(100))
        timer.setEventHandler { self.tick() }
        timer.resume()
        tickTimer = timer
    }

    /// Ogni millisecondo: il client fa il suo giro (ping, scadenze, statistiche), manda lo stato del tocco e prende i frame.
    private func tick() {
        let t = nowUs()
        for d in client.tick(nowUs: t) { sendUplink(d) }

        // Un dito finto che disegna cerchi, come farebbe il vero dito sul touchscreen.
        let phase = Double(t) / 1_000_000 * 2 * Double.pi
        let finger = TouchPoint(id: 1, x: UInt16(32768 + 20000 * cos(phase)), y: UInt16(32768 + 20000 * sin(phase)), pressure: 100)
        if let d = client.touchTick(nowUs: t, fingers: [finger]) {
            m.add { $0.touchSent += 1 }
            sendUplink(d)
        }
        if let state = host.takeTouch() {
            let age = Double(Int64(bitPattern: nowUs() &- state.clientTimeUs)) / 1000
            m.add {
                $0.touchReceived += 1
                $0.touchAgeMs.append(age)
            }
        }
        for f in client.takeFrames() { handleFrame(f) }
    }

    // MARK: Lato client: dal frame del core al livello di visualizzazione

    private func handleFrame(_ f: VideoFrame) {
        m.add {
            $0.delivered += 1
            if let l = f.latencyUs { $0.coreLatencyMs.append(Double(l) / 1000) }
        }
        let nals = AnnexB.splitNALs(f.data)
        let ps = AnnexB.parameterSets(from: nals, codec: .h265)
        if let vps = ps.vps, let sps = ps.sps, let pps = ps.pps {
            let sets = [vps, sps, pps]
            if formatDesc == nil || sets != lastParameterSets, let fmt = LoopbackRun.hevcFormat(sets) {
                formatDesc = fmt
                lastParameterSets = sets
            }
        }
        guard let fmt = formatDesc else { return } // si aspetta il primo keyframe
        let slices = nals.filter { !AnnexB.isParameterSet($0, codec: .h265) }
        guard !slices.isEmpty, let sb = LoopbackRun.makeSample(AnnexB.toLengthPrefixed(slices), format: fmt) else { return }
        m.add { $0.displayed += 1 }
        onSample(sb)
    }

    static func hevcFormat(_ sets: [Data]) -> CMVideoFormatDescription? {
        guard sets.count == 3 else { return nil }
        let copies = sets.map { [UInt8]($0) }
        guard copies.allSatisfy({ !$0.isEmpty }) else { return nil }
        let sizes = copies.map { $0.count }
        var fmt: CMVideoFormatDescription?
        let status: OSStatus = copies[0].withUnsafeBufferPointer { a in
            copies[1].withUnsafeBufferPointer { b in
                copies[2].withUnsafeBufferPointer { c in
                    let pointers: [UnsafePointer<UInt8>] = [a.baseAddress!, b.baseAddress!, c.baseAddress!]
                    return pointers.withUnsafeBufferPointer { pp in
                        sizes.withUnsafeBufferPointer { ss in
                            CMVideoFormatDescriptionCreateFromHEVCParameterSets(
                                allocator: kCFAllocatorDefault, parameterSetCount: 3,
                                parameterSetPointers: pp.baseAddress!, parameterSetSizes: ss.baseAddress!,
                                nalUnitHeaderLength: 4, extensions: nil, formatDescriptionOut: &fmt)
                        }
                    }
                }
            }
        }
        return status == noErr ? fmt : nil
    }

    /// NAL con prefisso di lunghezza in un CMSampleBuffer con «mostra subito», per la latenza minima.
    static func makeSample(_ avcc: Data, format: CMVideoFormatDescription) -> CMSampleBuffer? {
        var blockBuffer: CMBlockBuffer?
        var status = CMBlockBufferCreateWithMemoryBlock(
            allocator: kCFAllocatorDefault, memoryBlock: nil, blockLength: avcc.count,
            blockAllocator: kCFAllocatorDefault, customBlockSource: nil,
            offsetToData: 0, dataLength: avcc.count, flags: 0, blockBufferOut: &blockBuffer)
        guard status == kCMBlockBufferNoErr, let bb = blockBuffer else { return nil }
        status = avcc.withUnsafeBytes {
            CMBlockBufferReplaceDataBytes(with: $0.baseAddress!, blockBuffer: bb, offsetIntoDestination: 0, dataLength: avcc.count)
        }
        guard status == kCMBlockBufferNoErr else { return nil }

        var sample: CMSampleBuffer?
        var sizes = [avcc.count]
        status = CMSampleBufferCreateReady(
            allocator: kCFAllocatorDefault, dataBuffer: bb, formatDescription: format,
            sampleCount: 1, sampleTimingEntryCount: 0, sampleTimingArray: nil,
            sampleSizeEntryCount: 1, sampleSizeArray: &sizes, sampleBufferOut: &sample)
        guard status == noErr, let sb = sample else { return nil }

        if let attachments = CMSampleBufferGetSampleAttachmentsArray(sb, createIfNecessary: true) {
            let dict = unsafeBitCast(CFArrayGetValueAtIndex(attachments, 0), to: CFMutableDictionary.self)
            CFDictionarySetValue(dict,
                                 Unmanaged.passUnretained(kCMSampleAttachmentKey_DisplayImmediately).toOpaque(),
                                 Unmanaged.passUnretained(kCFBooleanTrue).toOpaque())
        }
        return sb
    }

    // MARK: Utilità

    private func sleepUntil(_ target: UInt64) {
        while true {
            let n = nowNs()
            if n >= target { return }
            let rem = target - n
            if rem > 2_000_000 { usleep(UInt32((rem - 1_000_000) / 1000)) }
        }
    }

    private func cpuSeconds() -> Double {
        var ru = rusage()
        getrusage(RUSAGE_SELF, &ru)
        let user = Double(ru.ru_utime.tv_sec) + Double(ru.ru_utime.tv_usec) / 1e6
        let system = Double(ru.ru_stime.tv_sec) + Double(ru.ru_stime.tv_usec) / 1e6
        return user + system
    }

    // MARK: Sorgente sintetica

    /// Immagini sintetiche con movimento globale e una fascia di rumore: l'encoder ha qualcosa di vero da comprimere e
    /// rispetta il bitrate che gli si chiede.
    final class Source {
        let width: Int
        let height: Int
        private var texture: [UInt8]
        private var noise: [UInt8]

        init(width: Int, height: Int) {
            self.width = width
            self.height = height
            let rows = height + 64
            var tex = [UInt8](repeating: 0, count: width * rows)
            for y in 0..<rows {
                for x in 0..<width {
                    tex[y * width + x] = UInt8(truncatingIfNeeded: x &* 3 &+ y &* 5 &+ ((x &* y) >> 7))
                }
            }
            var noiseBytes = [UInt8](repeating: 0, count: 1 << 20)
            var state: UInt32 = 0x1234_5678
            for i in 0..<noiseBytes.count {
                state = state &* 1_664_525 &+ 1_013_904_223 // generatore congruenziale: basta che sembri casuale
                noiseBytes[i] = UInt8(truncatingIfNeeded: state >> 16)
            }
            texture = tex
            noise = noiseBytes
        }

        func fill(_ pb: CVPixelBuffer, frame i: Int) {
            CVPixelBufferLockBaseAddress(pb, [])
            defer { CVPixelBufferUnlockBaseAddress(pb, []) }
            guard let ybase = CVPixelBufferGetBaseAddressOfPlane(pb, 0) else { return }
            let w = CVPixelBufferGetWidthOfPlane(pb, 0)
            let h = CVPixelBufferGetHeightOfPlane(pb, 0)
            let ystride = CVPixelBufferGetBytesPerRowOfPlane(pb, 0)
            let shift = (i * 5) % 64
            texture.withUnsafeBufferPointer { tex in
                guard let src = tex.baseAddress else { return }
                for y in 0..<h { memcpy(ybase + y * ystride, src + (y + shift) * width, min(w, width)) }
            }
            // Una fascia di 240 righe di rumore che scende lungo lo schermo.
            let band = 240
            let top = (i * 37) % max(1, h - band)
            noise.withUnsafeBufferPointer { nz in
                guard let src = nz.baseAddress else { return }
                for y in top..<(top + band) {
                    let offset = (y &* 131 &+ i &* 977) % (nz.count - w)
                    memcpy(ybase + y * ystride, src + offset, w)
                }
            }
            if let uv = CVPixelBufferGetBaseAddressOfPlane(pb, 1) {
                let uvh = CVPixelBufferGetHeightOfPlane(pb, 1)
                let uvstride = CVPixelBufferGetBytesPerRowOfPlane(pb, 1)
                memset(uv, Int32(96 + (i % 64)), uvh * uvstride) // il colore cambia piano piano
            }
        }
    }

    // MARK: Rapporto

    private func makeReport(wallSeconds: Double, cpuSeconds: Double, thermalStart: String) -> String {
        let now = nowUs()
        let hc = host.counters()
        let cc = client.counters(nowUs: now)
        let snap = m.read { m in
            (m.sourceFrames, m.encodedFrames, m.encodeFailures, m.keyFrames, m.delivered, m.displayed, m.datagramsSent,
             m.datagramsDropped, m.touchSent, m.touchReceived, m.encodedBytes)
        }
        let (source, encoded, failures, keyFrames, delivered, displayed, sent, dropped, touchSent, touchReceived, bytes) = snap
        let latency = BenchSummary(m.read { $0.coreLatencyMs })
        let encode = BenchSummary(m.read { $0.encodeMs })
        let sendUs = BenchSummary(m.read { $0.sendFrameUs })
        let rxUs = BenchSummary(m.read { $0.onDatagramUs })
        let touchAge = BenchSummary(m.read { $0.touchAgeMs })
        let trace = m.read { $0.bitrateTrace }

        var t = "Phantom F0 · prova «tutto sul telefono» (core Rust)\n"
        t += "modello \(DecoderBench.machineId()) · iOS \(UIDevice.current.systemVersion) · stato termico: \(thermalStart) → \(DecoderBench.thermal())\n"
        t += String(format: "HEVC %d×%d · %d fps · %d s · perdita simulata sulla discesa %.0f%% · orologio dell'host sfasato di +%.0f s\n\n",
                    s.width, s.height, s.fps, s.seconds, s.lossPercent, Double(hostClockOffsetUs) / 1e6)

        t += "frame: sorgente \(source) · codificati \(encoded) · consegnati dal core \(delivered) · mostrati \(displayed) · errori di codifica \(failures)\n"
        let lostPct = source > 0 ? 100 * Double(source - delivered) / Double(source) : 0
        t += String(format: "frame non arrivati: %d (%.1f%%) · persi dal core %llu · scartati in attesa di keyframe %llu · salvati dalla parità %llu\n",
                    max(0, source - delivered), lostPct, cc.framesLost, cc.framesDroppedWaitingKey, cc.framesRecoveredByParity)
        t += "latenza dalla cattura alla consegna del core: \(latency.text)\n"
        t += "  (comprende codifica, frammentazione, parità, perdite e ritrasmissioni; non la decodifica né lo schermo)\n"
        t += "codifica (invio → uscita dall'encoder): \(encode.text)\n\n"

        t += "costo del core sul telefono:\n"
        // (sendFrameUs e onDatagramUs sono già in microsecondi: BenchSummary non guarda l'unità)
        t += String(format: "  host.sendFrame per frame   p50 %.0f · p95 %.0f · max %.0f µs\n", sendUs.p50, sendUs.p95, sendUs.max)
        t += String(format: "  client.onDatagram          p50 %.0f · p95 %.0f · max %.0f µs per datagramma\n", rxUs.p50, rxUs.p95, rxUs.max)
        let cpuPct = wallSeconds > 0 ? 100 * cpuSeconds / wallSeconds : 0
        t += String(format: "  CPU di tutta l'app: %.0f%% di un core (sorgente, encoder, core e schermo insieme)\n\n", cpuPct)

        let minTrace = (trace.min() ?? 0) / 1e6, maxTrace = (trace.max() ?? 0) / 1e6
        let avgTrace = trace.isEmpty ? 0 : trace.reduce(0, +) / Double(trace.count) / 1e6
        let seconds = max(wallSeconds - 0.5, 0.1)
        t += String(format: "bitrate richiesto dal controllo: da %.1f a %.1f Mbps (medio %.1f) · uscito dall'encoder %.1f Mbps effettivi\n",
                    minTrace, maxTrace, avgTrace, Double(bytes) * 8 / seconds / 1e6)
        t += "keyframe: \(keyFrames) (richieste dal client \(cc.keyframeRequests), di cui ripetute \(cc.keyframeRetries)) · riduzioni del bitrate \(hc.bitrateDecreases)\n"
        let dropPct = sent > 0 ? 100 * Double(dropped) / Double(sent) : 0
        t += String(format: "datagrammi: %d inviati · %d persi dalla simulazione (%.1f%%) · ritrasmessi %llu (negati dal budget %llu) · NACK %llu\n",
                    sent, dropped, dropPct, hc.retransmitted, hc.retransmitDenied, cc.nacks)
        t += "ritardo di coda misurato dal client: \(cc.queueDelayUs.map { "\($0 / 1000) ms" } ?? "–") · RTT \(cc.rttUs.map { "\($0) µs" } ?? "–")\n"
        if let off = cc.clockOffsetUs {
            t += "orologi: offset stimato \(off) µs contro \(hostClockOffsetUs) reali (errore \(off - Int64(hostClockOffsetUs)) µs)\n\n"
        } else {
            t += "orologi: non sincronizzati\n\n"
        }

        t += "tocco: stati inviati \(touchSent) · ricevuti dall'host \(touchReceived) (\(hc.touchStates) contati dal core) · età alla ricezione \(touchAge.text)\n"
        return t
    }
}
