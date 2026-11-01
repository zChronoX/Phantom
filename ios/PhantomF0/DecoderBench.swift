import Foundation
import UIKit
import VideoToolbox
import CoreMedia
import CoreVideo

/// Banco di prova del decoder hardware dell'iPhone, senza il POCO.
///
/// Perché: nel budget di latenza di Phantom la codifica e la decodifica contano molto, e non sappiamo ancora quanto
/// costa la decodifica sull'iPhone 17 Pro Max a 1080×2400, a 60 e a 120 fps, in H.264 e in HEVC. Qui l'iPhone genera
/// frame sintetici, li codifica con il suo encoder hardware (a bassa latenza), li decodifica con lo STESSO tipo di
/// decoder che usa l'app per il video del POCO e misura i tempi. È una misura dell'iPhone, non del POCO.
final class DecoderBench: ObservableObject {
    struct Config {
        let name: String
        let codec: CMVideoCodecType
        let width: Int
        let height: Int
        let fps: Int
        let bitrate: Int
    }

    static let configs: [Config] = [
        Config(name: "H.264", codec: kCMVideoCodecType_H264, width: 1080, height: 2400, fps: 60, bitrate: 20_000_000),
        Config(name: "HEVC", codec: kCMVideoCodecType_HEVC, width: 1080, height: 2400, fps: 60, bitrate: 20_000_000),
        Config(name: "H.264", codec: kCMVideoCodecType_H264, width: 1080, height: 2400, fps: 120, bitrate: 20_000_000),
        Config(name: "HEVC", codec: kCMVideoCodecType_HEVC, width: 1080, height: 2400, fps: 120, bitrate: 20_000_000),
    ]

    @Published private(set) var running = false
    @Published private(set) var report = ""
    @Published private(set) var progress = ""

    func start() {
        guard !running else { return }
        running = true
        report = ""
        progress = "Preparazione…"
        UIApplication.shared.isIdleTimerDisabled = true
        DispatchQueue.global(qos: .userInitiated).async {
            var out = "Phantom F0 · banco di prova del decoder (iPhone)\n"
            out += "modello \(DecoderBench.machineId()) · iOS \(UIDevice.current.systemVersion) · stato termico all'inizio: \(DecoderBench.thermal())\n\n"
            for (i, c) in DecoderBench.configs.enumerated() {
                self.setProgress("Prova \(i + 1) di \(DecoderBench.configs.count): \(c.name) \(c.width)×\(c.height) a \(c.fps) fps…")
                out += self.runOne(c) + "\n"
            }
            out += "stato termico alla fine: \(DecoderBench.thermal())\n"
            DispatchQueue.main.async {
                self.report = out
                self.progress = "Finito"
                self.running = false
                UIApplication.shared.isIdleTimerDisabled = false
            }
        }
    }

    private func setProgress(_ text: String) {
        DispatchQueue.main.async { self.progress = text }
    }

    static func machineId() -> String {
        var info = utsname()
        uname(&info)
        return withUnsafePointer(to: &info.machine) {
            $0.withMemoryRebound(to: CChar.self, capacity: Int(_SYS_NAMELEN)) { String(cString: $0) }
        }
    }

    static func thermal() -> String {
        switch ProcessInfo.processInfo.thermalState {
        case .nominal: return "normale"
        case .fair: return "tiepido"
        case .serious: return "caldo (il sistema rallenta)"
        case .critical: return "critico"
        @unknown default: return "sconosciuto"
        }
    }

    private static func nowNs() -> UInt64 { DispatchTime.now().uptimeNanoseconds }

    /// Attende fino all'istante indicato: dorme quasi tutto il tempo e gira a vuoto nell'ultimo millisecondo.
    private static func sleepUntil(_ target: UInt64) {
        while true {
            let n = nowNs()
            if n >= target { return }
            let rem = target - n
            if rem > 2_000_000 { usleep(UInt32((rem - 1_000_000) / 1000)) }
        }
    }

    // MARK: una prova

    private func runOne(_ c: Config) -> String {
        let label = "\(c.name) · \(c.width)×\(c.height) · \(c.fps) fps · \(c.bitrate / 1_000_000) Mbps richiesti"
        let frames = c.fps * 3

        var encoder: VTCompressionSession?
        let lowLatency: [CFString: Any] = [kVTVideoEncoderSpecification_EnableLowLatencyRateControl: true]
        var st = VTCompressionSessionCreate(allocator: nil, width: Int32(c.width), height: Int32(c.height), codecType: c.codec,
                                            encoderSpecification: lowLatency as CFDictionary, imageBufferAttributes: nil,
                                            compressedDataAllocator: nil, outputCallback: nil, refcon: nil, compressionSessionOut: &encoder)
        if st != noErr || encoder == nil {
            // Il controllo di velocità a bassa latenza non è disponibile per tutti i codec: si riprova senza.
            st = VTCompressionSessionCreate(allocator: nil, width: Int32(c.width), height: Int32(c.height), codecType: c.codec,
                                            encoderSpecification: nil, imageBufferAttributes: nil, compressedDataAllocator: nil,
                                            outputCallback: nil, refcon: nil, compressionSessionOut: &encoder)
        }
        guard st == noErr, let session = encoder else { return "\(label)\n  impossibile creare l'encoder (errore \(st))\n" }

        VTSessionSetProperty(session, key: kVTCompressionPropertyKey_RealTime, value: kCFBooleanTrue)
        VTSessionSetProperty(session, key: kVTCompressionPropertyKey_AllowFrameReordering, value: kCFBooleanFalse)
        VTSessionSetProperty(session, key: kVTCompressionPropertyKey_ExpectedFrameRate, value: NSNumber(value: c.fps))
        VTSessionSetProperty(session, key: kVTCompressionPropertyKey_AverageBitRate, value: NSNumber(value: c.bitrate))
        VTSessionSetProperty(session, key: kVTCompressionPropertyKey_MaxKeyFrameInterval, value: NSNumber(value: c.fps * 2))
        VTSessionSetProperty(session, key: kVTCompressionPropertyKey_MaxFrameDelayCount, value: NSNumber(value: 0))
        VTCompressionSessionPrepareToEncodeFrames(session)
        defer { VTCompressionSessionInvalidate(session) }

        // Texture di partenza: più righe del necessario, così ogni frame la legge con uno scarto diverso (movimento globale).
        let w = c.width, h = c.height
        let texRows = h + 64
        var tex = [UInt8](repeating: 0, count: w * texRows)
        for y in 0..<texRows {
            for x in 0..<w {
                tex[y * w + x] = UInt8(truncatingIfNeeded: x &* 3 &+ y &* 5 &+ ((x &* y) >> 7))
            }
        }

        var pool = VTCompressionSessionGetPixelBufferPool(session)
        if pool == nil {
            let attrs: [CFString: Any] = [kCVPixelBufferPixelFormatTypeKey: kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange,
                                          kCVPixelBufferWidthKey: w, kCVPixelBufferHeightKey: h,
                                          kCVPixelBufferIOSurfacePropertiesKey: [String: Any]()]
            CVPixelBufferPoolCreate(nil, nil, attrs as CFDictionary, &pool)
        }
        guard let bufferPool = pool else { return "\(label)\n  nessun pool di buffer per l'encoder\n" }

        let lock = NSLock()
        var encMs = [Double](repeating: .nan, count: frames)
        var decMs = [Double](repeating: .nan, count: frames)
        var e2eMs = [Double](repeating: .nan, count: frames)
        var encoded = [CMSampleBuffer?](repeating: nil, count: frames)
        var totalBytes = 0
        var failures = 0
        var decoder: VTDecompressionSession?
        let group = DispatchGroup()

        let frameDuration = CMTime(value: 1, timescale: CMTimeScale(c.fps))
        let start = DecoderBench.nowNs() + 50_000_000
        for i in 0..<frames {
            DecoderBench.sleepUntil(start + UInt64(i) * 1_000_000_000 / UInt64(c.fps))

            var pbOut: CVPixelBuffer?
            guard CVPixelBufferPoolCreatePixelBuffer(nil, bufferPool, &pbOut) == kCVReturnSuccess, let pb = pbOut else {
                lock.lock(); failures += 1; lock.unlock()
                continue
            }
            tex.withUnsafeBufferPointer { DecoderBench.fill(pb, tex: $0, texStride: w, rowShift: (i * 5) % 64) }

            let props: CFDictionary? = i == 0 ? [kVTEncodeFrameOptionKey_ForceKeyFrame: true] as CFDictionary : nil
            let pts = CMTime(value: CMTimeValue(i), timescale: CMTimeScale(c.fps))
            let tSubmit = DecoderBench.nowNs()
            group.enter()
            let r = VTCompressionSessionEncodeFrame(session, imageBuffer: pb, presentationTimeStamp: pts, duration: frameDuration,
                                                    frameProperties: props, infoFlagsOut: nil) { status, _, sampleBuffer in
                let tEnc = DecoderBench.nowNs()
                guard status == noErr, let sb = sampleBuffer else {
                    lock.lock(); failures += 1; lock.unlock()
                    group.leave()
                    return
                }
                lock.lock()
                encMs[i] = Double(tEnc - tSubmit) / 1e6
                encoded[i] = sb
                totalBytes += CMSampleBufferGetTotalSampleSize(sb)
                if decoder == nil, let fd = CMSampleBufferGetFormatDescription(sb) {
                    var ds: VTDecompressionSession?
                    let hw: [CFString: Any] = [kVTVideoDecoderSpecification_EnableHardwareAcceleratedVideoDecoder: true]
                    if VTDecompressionSessionCreate(allocator: nil, formatDescription: fd, decoderSpecification: hw as CFDictionary,
                                                    imageBufferAttributes: nil, outputCallback: nil, decompressionSessionOut: &ds) == noErr {
                        decoder = ds
                    }
                }
                let ds = decoder
                lock.unlock()
                guard let dec = ds else {
                    lock.lock(); failures += 1; lock.unlock()
                    group.leave()
                    return
                }
                let tDec = DecoderBench.nowNs()
                let dr = VTDecompressionSessionDecodeFrame(dec, sampleBuffer: sb, flags: [._EnableAsynchronousDecompression],
                                                           infoFlagsOut: nil) { decStatus, _, image, _, _ in
                    let tDone = DecoderBench.nowNs()
                    lock.lock()
                    if decStatus == noErr && image != nil {
                        decMs[i] = Double(tDone - tDec) / 1e6
                        e2eMs[i] = Double(tDone - tSubmit) / 1e6
                    } else {
                        failures += 1
                    }
                    lock.unlock()
                    group.leave()
                }
                if dr != noErr {
                    lock.lock(); failures += 1; lock.unlock()
                    group.leave()
                }
            }
            if r != noErr {
                lock.lock(); failures += 1; lock.unlock()
                group.leave()
            }
        }
        VTCompressionSessionCompleteFrames(session, untilPresentationTimeStamp: CMTime.invalid)
        let waited = group.wait(timeout: .now() + 15)

        // Fase B: solo decodifica, il più in fretta possibile, sugli stessi frame già codificati.
        var maxFps = Double.nan
        lock.lock()
        let dec = decoder
        let stored = encoded
        lock.unlock()
        if let dec = dec {
            var outputs = 0
            let t0 = DecoderBench.nowNs()
            for sb in stored {
                guard let sb = sb else { continue }
                _ = VTDecompressionSessionDecodeFrame(dec, sampleBuffer: sb, flags: [._EnableAsynchronousDecompression], infoFlagsOut: nil) { s, _, image, _, _ in
                    if s == noErr && image != nil { lock.lock(); outputs += 1; lock.unlock() }
                }
            }
            VTDecompressionSessionWaitForAsynchronousFrames(dec)
            let secs = Double(DecoderBench.nowNs() - t0) / 1e9
            lock.lock()
            let n = outputs
            lock.unlock()
            if secs > 0 && n > 0 { maxFps = Double(n) / secs }
            VTDecompressionSessionInvalidate(dec)
        }

        let enc = BenchSummary(encMs), dc = BenchSummary(decMs), e2e = BenchSummary(e2eMs)
        let okFrames = e2e.count
        let avgKB = okFrames > 0 ? Double(totalBytes) / Double(max(okFrames, 1)) / 1024 : 0
        let mbps = Double(totalBytes) * 8 / 3.0 / 1e6
        var text = "\(label)\n"
        text += "  codifica            \(enc.text)\n"
        text += "  decodifica          \(dc.text)\n"
        text += "  codifica+decodifica \(e2e.text)\n"
        text += String(format: "  decodifica al massimo: %@ fps\n", maxFps.isNaN ? "–" : String(format: "%.0f", maxFps))
        text += String(format: "  frame riusciti %d su %d · medio %.0f KB (≈ %.1f Mbps effettivi) · errori %d%@\n", okFrames, frames, avgKB, mbps, failures,
                       waited == .timedOut ? " · tempo scaduto" : "")
        return text
    }

    /// Riempie un buffer NV12: piano Y copiato dalla texture (con scarto di righe), piano CbCr neutro.
    private static func fill(_ pb: CVPixelBuffer, tex: UnsafeBufferPointer<UInt8>, texStride: Int, rowShift: Int) {
        CVPixelBufferLockBaseAddress(pb, [])
        defer { CVPixelBufferUnlockBaseAddress(pb, []) }
        guard let ybase = CVPixelBufferGetBaseAddressOfPlane(pb, 0), let src = tex.baseAddress else { return }
        let h = CVPixelBufferGetHeightOfPlane(pb, 0)
        let w = CVPixelBufferGetWidthOfPlane(pb, 0)
        let ystride = CVPixelBufferGetBytesPerRowOfPlane(pb, 0)
        for y in 0..<h {
            memcpy(ybase + y * ystride, src + (y + rowShift) * texStride, w)
        }
        if let uvbase = CVPixelBufferGetBaseAddressOfPlane(pb, 1) {
            let uvh = CVPixelBufferGetHeightOfPlane(pb, 1)
            let uvstride = CVPixelBufferGetBytesPerRowOfPlane(pb, 1)
            for y in 0..<uvh { memset(uvbase + y * uvstride, 128, uvstride) }
        }
    }
}
