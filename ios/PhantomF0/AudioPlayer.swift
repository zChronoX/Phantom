import Foundation
import AVFoundation
import CoreMedia
import AudioToolbox

/// Riproduce i frame AAC-LC ricevuti dall'host Android tramite `AVSampleBufferAudioRenderer` +
/// `AVSampleBufferRenderSynchronizer` (decodifica hardware nativa iOS).
///
/// I pacchetti non vengono più suonati appena arrivano: passano dal buffer anti-jitter del core
/// (`AudioJitterBuffer`), che li mette al loro posto sulla linea del tempo (dall'istante di cattura dell'host),
/// scarta le copie di ridondanza, aspetta i ritardatari finché c'è tempo e decide quando far ripartire
/// l'orologio del riproduttore. Prima ogni pacchetto in ritardo diventava un buco o un salto (il gracchiare) e
/// l'audio partiva senza alcun margine.
final class AudioStreamPlayer {
    private let queue = DispatchQueue(label: "app.phantom.remote.audio", qos: .userInteractive)
    private let renderer = AVSampleBufferAudioRenderer()
    private let synchronizer = AVSampleBufferRenderSynchronizer()

    private var active = false
    private var formatDescription: CMAudioFormatDescription?
    private var lastSampleRate: UInt32 = 0
    private var lastChannels: UInt8 = 0
    private var lastAsc = Data()
    private var jitter: AudioJitterBuffer?
    private var clockRunning = false
    private var pumpTimer: DispatchSourceTimer?

    /// Campioni per pacchetto AAC-LC.
    private static let frameSamples: Int64 = 1024

    var isMuted: Bool {
        get { renderer.isMuted }
        set {
            queue.async { [weak self] in
                self?.renderer.isMuted = newValue
            }
        }
    }

    init() {
        synchronizer.addRenderer(renderer)
    }

    func start() {
        queue.async { [weak self] in
            guard let self = self, !self.active else { return }
            self.active = true
            self.formatDescription = nil
            self.lastAsc = Data()
            self.lastSampleRate = 0
            self.lastChannels = 0
            self.resetStream()
            do {
                let session = AVAudioSession.sharedInstance()
                try session.setCategory(.playback, mode: .default, options: [.mixWithOthers])
                try session.setActive(true)
            } catch {
                // La riproduzione audio è opzionale: non interrompe mai lo streaming video
            }
            // Il buffer va interrogato anche quando non arrivano pacchetti, per saltare in tempo quelli persi.
            let timer = DispatchSource.makeTimerSource(queue: self.queue)
            timer.schedule(deadline: .now() + .milliseconds(10), repeating: .milliseconds(10), leeway: .milliseconds(2))
            timer.setEventHandler { [weak self] in
                self?.pump()
            }
            self.pumpTimer = timer
            timer.resume()
        }
    }

    func stop() {
        queue.async { [weak self] in
            guard let self = self else { return }
            self.active = false
            self.pumpTimer?.cancel()
            self.pumpTimer = nil
            self.formatDescription = nil
            self.lastAsc = Data()
            self.resetStream()
        }
    }

    func enqueue(_ frame: AudioFrame) {
        queue.async { [weak self] in
            self?.handleFrame(frame)
        }
    }

    /// Contatori del buffer anti-jitter (per la telemetria); `nil` se l'audio non è ancora partito.
    func counters(_ completion: @escaping (JitterAudioCounters?) -> Void) {
        queue.async { [weak self] in
            completion(self?.jitter?.counters())
        }
    }

    private static func nowUs() -> UInt64 {
        DispatchTime.now().uptimeNanoseconds / 1_000
    }

    /// Ferma l'orologio, svuota il riproduttore e dimentica la linea del tempo (nuova sessione o formato nuovo).
    private func resetStream() {
        synchronizer.rate = 0
        renderer.flush()
        clockRunning = false
        jitter = nil
    }

    private func handleFrame(_ frame: AudioFrame) {
        guard active, !frame.data.isEmpty else { return }

        if frame.config {
            if frame.data != lastAsc || frame.sampleRate != lastSampleRate || frame.channels != lastChannels {
                lastAsc = frame.data
                lastSampleRate = frame.sampleRate
                lastChannels = frame.channels
                formatDescription = Self.makeFormatDescription(
                    sampleRate: frame.sampleRate,
                    channels: frame.channels,
                    asc: frame.data
                )
                resetStream()
            }
            return
        }

        // Senza AudioSpecificConfig il decoder AAC non sa cosa decodificare (prima si provava lo stesso e il
        // renderer falliva in silenzio): si aspetta, l'host lo ripete con i primi pacchetti e poi ogni secondo.
        guard formatDescription != nil, frame.sampleRate == lastSampleRate, frame.channels == lastChannels else {
            return
        }
        let buffer: AudioJitterBuffer
        if let existing = jitter {
            buffer = existing
        } else {
            buffer = AudioJitterBuffer(sampleRate: lastSampleRate)
            jitter = buffer
        }
        buffer.push(nowUs: Self.nowUs(), captureTsUs: frame.captureTsUs, data: frame.data)
        pump()
    }

    /// Chiede al buffer cosa accodare adesso e, se serve, fa ripartire l'orologio del riproduttore.
    private func pump() {
        guard active, let buffer = jitter, let fmt = formatDescription else { return }
        let sr = Int32(max(8_000, lastSampleRate))

        if renderer.status == .failed {
            // Il decoder si è bloccato: si svuota e si riparte (il buffer risincronizza se non ha la posizione).
            renderer.flush()
            clockRunning = false
        }

        var playhead: Int64?
        if clockRunning {
            let now = synchronizer.currentTime()
            if now.isNumeric {
                playhead = CMTimeConvertScale(now, timescale: sr, method: .roundTowardNegativeInfinity).value
            }
        }

        let result = buffer.poll(nowUs: Self.nowUs(), playheadSamples: playhead)
        if let start = result.resyncPlayheadSamples {
            renderer.flush()
            synchronizer.setRate(1.0, time: CMTime(value: start, timescale: sr))
            clockRunning = true
        }

        let duration = CMTime(value: Self.frameSamples, timescale: sr)
        for frame in result.frames {
            guard renderer.isReadyForMoreMediaData else { break }
            let pts = CMTime(value: frame.ptsSamples, timescale: sr)
            if let sb = Self.makeSampleBuffer(data: frame.data, format: fmt, pts: pts, duration: duration) {
                renderer.enqueue(sb)
            }
        }
    }

    private static func makeFormatDescription(
        sampleRate: UInt32,
        channels: UInt8,
        asc: Data?
    ) -> CMAudioFormatDescription? {
        var asbd = AudioStreamBasicDescription(
            mSampleRate: Float64(sampleRate),
            mFormatID: kAudioFormatMPEG4AAC,
            mFormatFlags: UInt32(MPEG4ObjectID.AAC_LC.rawValue),
            mBytesPerPacket: 0,
            mFramesPerPacket: 1024,
            mBytesPerFrame: 0,
            mChannelsPerFrame: UInt32(channels),
            mBitsPerChannel: 0,
            mReserved: 0
        )

        if let asc = asc, !asc.isEmpty {
            let esds = buildEsdsMagicCookie(from: asc)
            var desc: CMAudioFormatDescription?
            let status: OSStatus = esds.withUnsafeBytes { raw in
                CMAudioFormatDescriptionCreate(
                    allocator: kCFAllocatorDefault,
                    asbd: &asbd,
                    layoutSize: 0,
                    layout: nil,
                    magicCookieSize: esds.count,
                    magicCookie: raw.baseAddress,
                    extensions: nil,
                    formatDescriptionOut: &desc
                )
            }
            if status == noErr, let desc = desc {
                return desc
            }
        }

        var fallback: CMAudioFormatDescription?
        let status = CMAudioFormatDescriptionCreate(
            allocator: kCFAllocatorDefault,
            asbd: &asbd,
            layoutSize: 0,
            layout: nil,
            magicCookieSize: 0,
            magicCookie: nil,
            extensions: nil,
            formatDescriptionOut: &fallback
        )
        return status == noErr ? fallback : nil
    }

    /// Costruisce un descrittore ISO 14496-1 `ES_Descriptor` (`esds`) attorno all'`AudioSpecificConfig` di MediaCodec.
    static func buildEsdsMagicCookie(from asc: Data) -> Data {
        let ascCount = min(asc.count, 127)
        let decSpec: [UInt8] = [0x05, UInt8(ascCount)] + Array(asc.prefix(ascCount))
        let decCfgBody: [UInt8] = [
            0x40, // MPEG-4 Audio
            0x15, // AudioStream
            0x00, 0x00, 0x00, // bufferSizeDB
            0x00, 0x02, 0xEE, 0x00, // maxBitrate (~192 kbps)
            0x00, 0x01, 0xF4, 0x00, // avgBitrate (~128 kbps)
        ] + decSpec
        let decCfg: [UInt8] = [0x04, UInt8(decCfgBody.count)] + decCfgBody
        let slCfg: [UInt8] = [0x06, 0x01, 0x02]
        let esBody: [UInt8] = [0x00, 0x00, 0x00] + decCfg + slCfg
        return Data([0x03, UInt8(esBody.count)] + esBody)
    }

    private static func makeSampleBuffer(
        data: Data,
        format: CMAudioFormatDescription,
        pts: CMTime,
        duration: CMTime
    ) -> CMSampleBuffer? {
        var blockBuffer: CMBlockBuffer?
        let createStatus = CMBlockBufferCreateWithMemoryBlock(
            allocator: kCFAllocatorDefault,
            memoryBlock: nil,
            blockLength: data.count,
            blockAllocator: kCFAllocatorDefault,
            customBlockSource: nil,
            offsetToData: 0,
            dataLength: data.count,
            flags: 0,
            blockBufferOut: &blockBuffer
        )
        guard createStatus == kCMBlockBufferNoErr, let block = blockBuffer else { return nil }

        let copyStatus: OSStatus = data.withUnsafeBytes { raw in
            guard let base = raw.baseAddress else { return -1 }
            return CMBlockBufferReplaceDataBytes(
                with: base,
                blockBuffer: block,
                offsetIntoDestination: 0,
                dataLength: data.count
            )
        }
        guard copyStatus == kCMBlockBufferNoErr else { return nil }

        var timing = CMSampleTimingInfo(duration: duration, presentationTimeStamp: pts, decodeTimeStamp: .invalid)
        var size = data.count
        var sb: CMSampleBuffer?
        let sbStatus = CMSampleBufferCreateReady(
            allocator: kCFAllocatorDefault,
            dataBuffer: block,
            formatDescription: format,
            sampleCount: 1,
            sampleTimingEntryCount: 1,
            sampleTimingArray: &timing,
            sampleSizeEntryCount: 1,
            sampleSizeArray: &size,
            sampleBufferOut: &sb
        )
        return sbStatus == noErr ? sb : nil
    }
}
