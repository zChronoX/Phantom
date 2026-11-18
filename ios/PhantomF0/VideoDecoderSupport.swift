import Foundation
import CoreMedia
import VideoToolbox

/// Converte i frame Annex-B consegnati da `PhantomClient` (H.264 o HEVC) in `CMSampleBuffer` pronti per
/// `AVSampleBufferDisplayLayer`, tenendo traccia di format description e dimensioni del video.
final class AnnexBSampleBuilder {
    private var lastCodec: VideoCodec?
    private var lastParameterSets: [Data] = []
    private(set) var formatDescription: CMVideoFormatDescription?
    private(set) var videoWidth: Int = 0
    private(set) var videoHeight: Int = 0

    func reset() {
        lastCodec = nil
        lastParameterSets.removeAll()
        formatDescription = nil
        videoWidth = 0
        videoHeight = 0
    }

    func sampleBuffer(from annexBData: Data, codec: VideoCodec) -> CMSampleBuffer? {
        if lastCodec != codec {
            reset()
            lastCodec = codec
        }
        let scrcpyCodec: ScrcpyCodec = codec == .h265 ? .h265 : .h264
        let nals = AnnexB.splitNALs(annexBData)
        let ps = AnnexB.parameterSets(from: nals, codec: scrcpyCodec)

        let sets: [Data]?
        switch codec {
        case .h265:
            if let vps = ps.vps, let sps = ps.sps, let pps = ps.pps {
                sets = [vps, sps, pps]
            } else {
                sets = nil
            }
        case .h264:
            if let sps = ps.sps, let pps = ps.pps {
                sets = [sps, pps]
            } else {
                sets = nil
            }
        }

        if let sets = sets, (formatDescription == nil || sets != lastParameterSets) {
            if let fmt = Self.makeFormatDescription(sets: sets, codec: codec) {
                formatDescription = fmt
                lastParameterSets = sets
                let dims = CMVideoFormatDescriptionGetDimensions(fmt)
                videoWidth = Int(dims.width)
                videoHeight = Int(dims.height)
            }
        }

        guard let fmt = formatDescription else { return nil }
        let slices = nals.filter { !AnnexB.isParameterSet($0, codec: scrcpyCodec) }
        guard !slices.isEmpty else { return nil }
        let avcc = AnnexB.toLengthPrefixed(slices)
        return Self.makeSample(avcc, format: fmt)
    }

    private static func makeFormatDescription(sets: [Data], codec: VideoCodec) -> CMVideoFormatDescription? {
        let copies = sets.map { [UInt8]($0) }
        guard copies.allSatisfy({ !$0.isEmpty }) else { return nil }
        let sizes = copies.map(\.count)
        var fmt: CMVideoFormatDescription?

        switch codec {
        case .h265:
            guard copies.count == 3 else { return nil }
            let status: OSStatus = copies[0].withUnsafeBufferPointer { a in
                copies[1].withUnsafeBufferPointer { b in
                    copies[2].withUnsafeBufferPointer { c in
                        let pointers: [UnsafePointer<UInt8>] = [a.baseAddress!, b.baseAddress!, c.baseAddress!]
                        return pointers.withUnsafeBufferPointer { pp in
                            sizes.withUnsafeBufferPointer { ss in
                                CMVideoFormatDescriptionCreateFromHEVCParameterSets(
                                    allocator: kCFAllocatorDefault,
                                    parameterSetCount: 3,
                                    parameterSetPointers: pp.baseAddress!,
                                    parameterSetSizes: ss.baseAddress!,
                                    nalUnitHeaderLength: 4,
                                    extensions: nil,
                                    formatDescriptionOut: &fmt
                                )
                            }
                        }
                    }
                }
            }
            return status == noErr ? fmt : nil

        case .h264:
            guard copies.count == 2 else { return nil }
            let status: OSStatus = copies[0].withUnsafeBufferPointer { a in
                copies[1].withUnsafeBufferPointer { b in
                    let pointers: [UnsafePointer<UInt8>] = [a.baseAddress!, b.baseAddress!]
                    return pointers.withUnsafeBufferPointer { pp in
                        sizes.withUnsafeBufferPointer { ss in
                            CMVideoFormatDescriptionCreateFromH264ParameterSets(
                                allocator: kCFAllocatorDefault,
                                parameterSetCount: 2,
                                parameterSetPointers: pp.baseAddress!,
                                parameterSetSizes: ss.baseAddress!,
                                nalUnitHeaderLength: 4,
                                formatDescriptionOut: &fmt
                            )
                        }
                    }
                }
            }
            return status == noErr ? fmt : nil
        }
    }

    private static func makeSample(_ avcc: Data, format: CMVideoFormatDescription) -> CMSampleBuffer? {
        var blockBuffer: CMBlockBuffer?
        var status = CMBlockBufferCreateWithMemoryBlock(
            allocator: kCFAllocatorDefault,
            memoryBlock: nil,
            blockLength: avcc.count,
            blockAllocator: kCFAllocatorDefault,
            customBlockSource: nil,
            offsetToData: 0,
            dataLength: avcc.count,
            flags: 0,
            blockBufferOut: &blockBuffer
        )
        guard status == kCMBlockBufferNoErr, let bb = blockBuffer else { return nil }
        status = avcc.withUnsafeBytes {
            CMBlockBufferReplaceDataBytes(
                with: $0.baseAddress!,
                blockBuffer: bb,
                offsetIntoDestination: 0,
                dataLength: avcc.count
            )
        }
        guard status == kCMBlockBufferNoErr else { return nil }

        var sample: CMSampleBuffer?
        var sizes = [avcc.count]
        status = CMSampleBufferCreateReady(
            allocator: kCFAllocatorDefault,
            dataBuffer: bb,
            formatDescription: format,
            sampleCount: 1,
            sampleTimingEntryCount: 0,
            sampleTimingArray: nil,
            sampleSizeEntryCount: 1,
            sampleSizeArray: &sizes,
            sampleBufferOut: &sample
        )
        guard status == noErr, let sb = sample else { return nil }

        if let attachments = CMSampleBufferGetSampleAttachmentsArray(sb, createIfNecessary: true) {
            let dict = unsafeBitCast(CFArrayGetValueAtIndex(attachments, 0), to: CFMutableDictionary.self)
            CFDictionarySetValue(
                dict,
                Unmanaged.passUnretained(kCMSampleAttachmentKey_DisplayImmediately).toOpaque(),
                Unmanaged.passUnretained(kCFBooleanTrue).toOpaque()
            )
        }
        return sb
    }
}
