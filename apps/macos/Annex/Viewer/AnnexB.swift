import CoreMedia
import Foundation

/// H.264 byte-stream helpers. The wire format is Annex B (start codes); VideoToolbox and
/// CoreMedia use AVCC (4-byte big-endian length prefixes).
enum AnnexB {
    static let startCode: [UInt8] = [0, 0, 0, 1]

    /// Splits an Annex B buffer into NAL units (without start codes).
    static func nalUnits(_ bytes: [UInt8]) -> [ArraySlice<UInt8>] {
        var starts: [Int] = []
        var i = 0
        while i + 3 <= bytes.count {
            if bytes[i] == 0, bytes[i + 1] == 0, bytes[i + 2] == 1 {
                starts.append(i + 3)
                i += 3
            } else {
                i += 1
            }
        }
        var out: [ArraySlice<UInt8>] = []
        for (k, s) in starts.enumerated() {
            var end = k + 1 < starts.count ? starts[k + 1] - 3 : bytes.count
            while k + 1 < starts.count, end > s, bytes[end - 1] == 0 { end -= 1 }
            if end > s { out.append(bytes[s..<end]) }
        }
        return out
    }

    static func nalType(_ nal: ArraySlice<UInt8>) -> UInt8 {
        (nal.first ?? 0) & 0x1F
    }

    /// Converts length-prefixed NAL units to Annex B. Returns nil on malformed input.
    static func fromAVCC(_ avcc: [UInt8], lengthSize: Int = 4) -> [UInt8]? {
        var out: [UInt8] = []
        out.reserveCapacity(avcc.count + 16)
        var i = 0
        while i < avcc.count {
            guard i + lengthSize <= avcc.count else { return nil }
            var len = 0
            for b in avcc[i..<i + lengthSize] { len = (len << 8) | Int(b) }
            i += lengthSize
            guard len > 0, i + len <= avcc.count else { return nil }
            out += startCode
            out += avcc[i..<i + len]
            i += len
        }
        return out
    }

    /// Annex B → AVCC, dropping parameter sets and delimiters (they live in the format
    /// description).
    static func toAVCC(_ nals: [ArraySlice<UInt8>]) -> [UInt8] {
        var out: [UInt8] = []
        for nal in nals where ![7, 8, 9].contains(nalType(nal)) {
            let n = UInt32(nal.count)
            out += [UInt8(n >> 24), UInt8((n >> 16) & 0xFF), UInt8((n >> 8) & 0xFF), UInt8(n & 0xFF)]
            out += nal
        }
        return out
    }

    /// Encoder output → (Annex B access unit, is keyframe). Keyframes are prefixed with
    /// SPS/PPS so a viewer can start decoding from any keyframe.
    static func fromSampleBuffer(_ sb: CMSampleBuffer) -> (Data, Bool)? {
        guard let block = CMSampleBufferGetDataBuffer(sb),
              let format = CMSampleBufferGetFormatDescription(sb) else { return nil }
        var isKey = true
        if let attachments = CMSampleBufferGetSampleAttachmentsArray(sb, createIfNecessary: false) as? [[CFString: Any]],
           let first = attachments.first,
           let notSync = first[kCMSampleAttachmentKey_NotSync] as? Bool {
            isKey = !notSync
        }
        var out: [UInt8] = []
        var nalLength: Int32 = 4
        if isKey {
            var count = 0
            CMVideoFormatDescriptionGetH264ParameterSetAtIndex(format, parameterSetIndex: 0, parameterSetPointerOut: nil,
                                                               parameterSetSizeOut: nil, parameterSetCountOut: &count,
                                                               nalUnitHeaderLengthOut: &nalLength)
            for idx in 0..<count {
                var ptr: UnsafePointer<UInt8>?
                var size = 0
                if CMVideoFormatDescriptionGetH264ParameterSetAtIndex(format, parameterSetIndex: idx, parameterSetPointerOut: &ptr,
                                                                      parameterSetSizeOut: &size, parameterSetCountOut: nil,
                                                                      nalUnitHeaderLengthOut: nil) == noErr, let ptr {
                    out += startCode
                    out += UnsafeBufferPointer(start: ptr, count: size)
                }
            }
        }
        let length = CMBlockBufferGetDataLength(block)
        var avcc = [UInt8](repeating: 0, count: length)
        guard CMBlockBufferCopyDataBytes(block, atOffset: 0, dataLength: length, destination: &avcc) == noErr,
              let annexB = fromAVCC(avcc, lengthSize: Int(nalLength)) else { return nil }
        out += annexB
        return (Data(out), isKey)
    }
}

/// Turns received Annex B access units into CMSampleBuffers for AVSampleBufferDisplayLayer
/// or VTDecompressionSession, tracking SPS/PPS changes.
struct H264SampleBuilder {
    private var sps: [UInt8]?
    private var pps: [UInt8]?
    private(set) var format: CMVideoFormatDescription?

    var dimensions: CMVideoDimensions? {
        format.map { CMVideoFormatDescriptionGetDimensions($0) }
    }

    /// Returns nil until a keyframe with parameter sets has been seen.
    mutating func sampleBuffer(from data: Data) -> CMSampleBuffer? {
        let bytes = [UInt8](data)
        let nals = AnnexB.nalUnits(bytes)
        var paramsChanged = false
        for nal in nals {
            switch AnnexB.nalType(nal) {
            case 7: if sps != Array(nal) { sps = Array(nal); paramsChanged = true }
            case 8: if pps != Array(nal) { pps = Array(nal); paramsChanged = true }
            default: break
            }
        }
        if paramsChanged || format == nil, let sps, let pps {
            format = Self.makeFormat(sps: sps, pps: pps)
        }
        guard let format else { return nil }
        let avcc = AnnexB.toAVCC(nals)
        guard !avcc.isEmpty else { return nil }

        var block: CMBlockBuffer?
        guard CMBlockBufferCreateWithMemoryBlock(allocator: kCFAllocatorDefault, memoryBlock: nil, blockLength: avcc.count,
                                                 blockAllocator: kCFAllocatorDefault, customBlockSource: nil, offsetToData: 0,
                                                 dataLength: avcc.count, flags: 0, blockBufferOut: &block) == noErr,
              let block,
              avcc.withUnsafeBytes({ CMBlockBufferReplaceDataBytes(with: $0.baseAddress!, blockBuffer: block, offsetIntoDestination: 0, dataLength: avcc.count) }) == noErr
        else { return nil }

        var sample: CMSampleBuffer?
        var size = avcc.count
        guard CMSampleBufferCreateReady(allocator: kCFAllocatorDefault, dataBuffer: block, formatDescription: format, sampleCount: 1,
                                        sampleTimingEntryCount: 0, sampleTimingArray: nil, sampleSizeEntryCount: 1,
                                        sampleSizeArray: &size, sampleBufferOut: &sample) == noErr,
              let sample else { return nil }
        if let attachments = CMSampleBufferGetSampleAttachmentsArray(sample, createIfNecessary: true),
           CFArrayGetCount(attachments) > 0 {
            let dict = unsafeBitCast(CFArrayGetValueAtIndex(attachments, 0), to: CFMutableDictionary.self)
            CFDictionarySetValue(dict, Unmanaged.passUnretained(kCMSampleAttachmentKey_DisplayImmediately).toOpaque(),
                                 Unmanaged.passUnretained(kCFBooleanTrue).toOpaque())
        }
        return sample
    }

    static func makeFormat(sps: [UInt8], pps: [UInt8]) -> CMVideoFormatDescription? {
        var format: CMVideoFormatDescription?
        let status = sps.withUnsafeBufferPointer { s in
            pps.withUnsafeBufferPointer { p in
                let pointers = [s.baseAddress!, p.baseAddress!]
                let sizes = [s.count, p.count]
                return CMVideoFormatDescriptionCreateFromH264ParameterSets(allocator: kCFAllocatorDefault, parameterSetCount: 2,
                                                                           parameterSetPointers: pointers, parameterSetSizes: sizes,
                                                                           nalUnitHeaderLength: 4, formatDescriptionOut: &format)
            }
        }
        return status == noErr ? format : nil
    }
}
