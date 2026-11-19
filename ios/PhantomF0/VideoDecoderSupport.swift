import Foundation
import CoreMedia
import VideoToolbox

/// Risultato della preparazione di un frame pronto per `AVSampleBufferRenderer`.
struct BuiltVideoSample {
    let sampleBuffer: CMSampleBuffer
    let dimensions: VideoDimensions
    let formatChanged: Bool
}

/// Converte i frame Annex-B consegnati da `PhantomClient` (H.264 o HEVC) in `CMSampleBuffer` pronti per
/// `AVSampleBufferDisplayLayer`, con copia diretta Annex-B → `CMBlockBuffer` per ridurre le allocazioni a 120 fps.
final class AnnexBSampleBuilder {
    private var lastCodec: VideoCodec?
    private var lastParameterSets: [Data] = []
    private(set) var formatDescription: CMVideoFormatDescription?
    private(set) var dimensions = VideoDimensions.zero

    func reset() {
        lastCodec = nil
        lastParameterSets.removeAll()
        formatDescription = nil
        dimensions = .zero
    }

    func buildSample(from annexBData: Data, codec: VideoCodec) -> BuiltVideoSample? {
        if lastCodec != codec {
            reset()
            lastCodec = codec
        }
        let ranges = AnnexBScanner.nalRanges(in: annexBData)
        guard !ranges.isEmpty else { return nil }

        let isHEVC = codec == .h265
        var vps: Data?
        var sps: Data?
        var pps: Data?
        var sliceRanges: [Range<Int>] = []
        sliceRanges.reserveCapacity(ranges.count)

        annexBData.withUnsafeBytes { raw in
            guard let base = raw.bindMemory(to: UInt8.self).baseAddress else { return }
            for range in ranges {
                guard !range.isEmpty else { continue }
                let firstByte = base[range.lowerBound]
                if let kind = AnnexBScanner.parameterSetKind(firstByte: firstByte, isHEVC: isHEVC) {
                    let psData = Data(bytes: base + range.lowerBound, count: range.count)
                    switch kind {
                    case .vps: vps = psData
                    case .sps: sps = psData
                    case .pps: pps = psData
                    }
                } else {
                    sliceRanges.append(range)
                }
            }
        }

        let sets: [Data]?
        switch codec {
        case .h265:
            if let v = vps, let s = sps, let p = pps {
                sets = [v, s, p]
            } else {
                sets = nil
            }
        case .h264:
            if let s = sps, let p = pps {
                sets = [s, p]
            } else {
                sets = nil
            }
        }

        var formatChanged = false
        if let sets = sets, (formatDescription == nil || sets != lastParameterSets) {
            if let fmt = Self.makeFormatDescription(sets: sets, codec: codec) {
                formatChanged = formatDescription != nil
                formatDescription = fmt
                lastParameterSets = sets
                let dims = CMVideoFormatDescriptionGetDimensions(fmt)
                dimensions = VideoDimensions(width: Int(dims.width), height: Int(dims.height))
            }
        }

        guard let fmt = formatDescription, !sliceRanges.isEmpty else { return nil }
        guard let sb = Self.makeSample(from: annexBData, sliceRanges: sliceRanges, format: fmt) else { return nil }
        return BuiltVideoSample(sampleBuffer: sb, dimensions: dimensions, formatChanged: formatChanged)
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

    private static func makeSample(
        from annexBData: Data,
        sliceRanges: [Range<Int>],
        format: CMVideoFormatDescription
    ) -> CMSampleBuffer? {
        let totalLength = sliceRanges.reduce(0) { $0 + 4 + $1.count }
        guard totalLength > 0 else { return nil }

        var blockBuffer: CMBlockBuffer?
        var status = CMBlockBufferCreateWithMemoryBlock(
            allocator: kCFAllocatorDefault,
            memoryBlock: nil,
            blockLength: totalLength,
            blockAllocator: kCFAllocatorDefault,
            customBlockSource: nil,
            offsetToData: 0,
            dataLength: totalLength,
            flags: CMBlockBufferFlags(kCMBlockBufferAssureMemoryNowFlag),
            blockBufferOut: &blockBuffer
        )
        guard status == kCMBlockBufferNoErr, let bb = blockBuffer else { return nil }

        var lenAtOffset = 0
        var dataPtr: UnsafeMutablePointer<CChar>?
        status = CMBlockBufferGetDataPointer(
            bb,
            atOffset: 0,
            lengthAtOffsetOut: &lenAtOffset,
            totalLengthOut: nil,
            dataPointerOut: &dataPtr
        )
        guard status == kCMBlockBufferNoErr, lenAtOffset >= totalLength, let dst = dataPtr else { return nil }

        let copiedAll: Bool = annexBData.withUnsafeBytes { raw in
            guard let srcBase = raw.baseAddress else { return false }
            var offset = 0
            for range in sliceRanges {
                var beLen = UInt32(range.count).bigEndian
                withUnsafeBytes(of: &beLen) { lenRaw in
                    memcpy(dst + offset, lenRaw.baseAddress!, 4)
                }
                offset += 4
                memcpy(dst + offset, srcBase + range.lowerBound, range.count)
                offset += range.count
            }
            return offset == totalLength
        }
        guard copiedAll else { return nil }

        var sample: CMSampleBuffer?
        var sizes = [totalLength]
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
