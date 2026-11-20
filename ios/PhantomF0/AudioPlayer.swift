import Foundation
import AVFoundation
import CoreMedia
import AudioToolbox

/// Riproduce i frame AAC-LC ricevuti dall'host Android a bassissima latenza tramite
/// `AVSampleBufferAudioRenderer` + `AVSampleBufferRenderSynchronizer` (decodifica hardware nativa iOS).
final class AudioStreamPlayer {
    private let queue = DispatchQueue(label: "app.phantom.remote.audio", qos: .userInteractive)
    private let renderer = AVSampleBufferAudioRenderer()
    private let synchronizer = AVSampleBufferRenderSynchronizer()

    private var active = false
    private var formatDescription: CMAudioFormatDescription?
    private var lastSampleRate: UInt32 = 0
    private var lastChannels: UInt8 = 0
    private var lastAsc = Data()
    private var nextFrameIndex: Int64 = 0
    private var startedClock = false

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
            self.nextFrameIndex = 0
            self.startedClock = false
            do {
                let session = AVAudioSession.sharedInstance()
                try session.setCategory(.playback, mode: .default, options: [.mixWithOthers])
                try session.setActive(true)
            } catch {
                // La riproduzione audio è opzionale: non interrompe mai lo streaming video
            }
        }
    }

    func stop() {
        queue.async { [weak self] in
            guard let self = self else { return }
            self.active = false
            self.startedClock = false
            self.formatDescription = nil
            self.lastAsc = Data()
            self.nextFrameIndex = 0
            self.synchronizer.rate = 0
            self.renderer.flush()
        }
    }

    func enqueue(_ frame: AudioFrame) {
        queue.async { [weak self] in
            self?.handleFrame(frame)
        }
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
            }
            return
        }

        if formatDescription == nil || frame.sampleRate != lastSampleRate || frame.channels != lastChannels {
            lastSampleRate = frame.sampleRate
            lastChannels = frame.channels
            formatDescription = Self.makeFormatDescription(
                sampleRate: frame.sampleRate,
                channels: frame.channels,
                asc: lastAsc.isEmpty ? nil : lastAsc
            )
        }
        guard let fmt = formatDescription else { return }

        if renderer.status == .failed {
            renderer.flush()
            startedClock = false
            nextFrameIndex = 0
        }
        guard renderer.isReadyForMoreMediaData else { return }

        let sr = Int32(max(8_000, frame.sampleRate))
        let frameSamples: Int64 = 1024
        let pts = CMTime(value: nextFrameIndex * frameSamples, timescale: sr)
        let dur = CMTime(value: frameSamples, timescale: sr)
        nextFrameIndex += 1

        guard let sb = Self.makeSampleBuffer(data: frame.data, format: fmt, pts: pts, duration: dur) else { return }

        if !startedClock {
            startedClock = true
            synchronizer.setRate(1.0, time: pts)
        } else {
            let current = synchronizer.currentTime()
            if current.isNumeric {
                let drift = CMTimeGetSeconds(CMTimeSubtract(pts, current))
                if drift > 0.25 || drift < -0.15 {
                    renderer.flush()
                    synchronizer.setRate(1.0, time: pts)
                }
            }
        }
        renderer.enqueue(sb)
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
