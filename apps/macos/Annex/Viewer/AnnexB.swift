import CoreMedia
import Foundation
import VideoToolbox

/// H.264 and HEVC byte-stream helpers. The wire format is Annex B (start codes); VideoToolbox
/// and CoreMedia use length prefixes (4-byte big-endian).
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

    /// H.264 NAL unit type (5 bits).
    static func nalType(_ nal: ArraySlice<UInt8>) -> UInt8 {
        (nal.first ?? 0) & 0x1F
    }

    /// HEVC NAL unit type (6 bits after the forbidden bit; the header is two bytes).
    static func hevcNalType(_ nal: ArraySlice<UInt8>) -> UInt8 {
        ((nal.first ?? 0) >> 1) & 0x3F
    }

    /// An HEVC VPS (type 32, layer 0): only HEVC keyframes carry one, so it tells the codecs apart.
    static func isHevcVPS(_ nal: ArraySlice<UInt8>) -> Bool {
        nal.count >= 2 && nal[nal.startIndex] == 0x40 && nal[nal.startIndex + 1] == 0x01
    }

    /// An H.264 SPS (type 7).
    static func isH264SPS(_ nal: ArraySlice<UInt8>) -> Bool {
        guard let b = nal.first else { return false }
        return b & 0x80 == 0 && b & 0x1F == 7
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

    /// Annex B → length-prefixed, dropping parameter sets and delimiters (they live in the format
    /// description): H.264 SPS/PPS/AUD, or HEVC VPS/SPS/PPS/AUD.
    static func toAVCC(_ nals: [ArraySlice<UInt8>], hevc: Bool = false) -> [UInt8] {
        var out: [UInt8] = []
        for nal in nals where hevc ? ![32, 33, 34, 35].contains(hevcNalType(nal)) : ![7, 8, 9].contains(nalType(nal)) {
            let n = UInt32(nal.count)
            out += [UInt8(n >> 24), UInt8((n >> 16) & 0xFF), UInt8((n >> 8) & 0xFF), UInt8(n & 0xFF)]
            out += nal
        }
        return out
    }

    /// Encoder output → (Annex B access unit, is keyframe). Keyframes are prefixed with the
    /// parameter sets (H.264 SPS/PPS, HEVC VPS/SPS/PPS) so a viewer can start from any keyframe.
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
        let hevc = CMFormatDescriptionGetMediaSubType(format) == kCMVideoCodecType_HEVC
        // The two codecs' parameter-set getters have the same shape.
        typealias Getter = (CMFormatDescription, Int, UnsafeMutablePointer<UnsafePointer<UInt8>?>?, UnsafeMutablePointer<Int>?,
                            UnsafeMutablePointer<Int>?, UnsafeMutablePointer<Int32>?) -> OSStatus
        let get: Getter = hevc ? CMVideoFormatDescriptionGetHEVCParameterSetAtIndex : CMVideoFormatDescriptionGetH264ParameterSetAtIndex
        if isKey {
            var count = 0
            _ = get(format, 0, nil, nil, &count, &nalLength)
            for idx in 0..<count {
                var ptr: UnsafePointer<UInt8>?
                var size = 0
                if get(format, idx, &ptr, &size, nil, nil) == noErr, let ptr {
                    out += startCode
                    out += UnsafeBufferPointer(start: ptr, count: size)
                }
            }
        } else {
            _ = get(format, 0, nil, nil, nil, &nalLength)
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
/// or VTDecompressionSession, tracking parameter-set changes. H.264 or HEVC: it follows the
/// codec of the last keyframe's parameter sets, so a stream that switches codec carries on.
struct H264SampleBuilder {
    private var vps: [UInt8]?
    private var sps: [UInt8]?
    private var pps: [UInt8]?
    private(set) var format: CMVideoFormatDescription?
    /// The stream is HEVC (else H.264).
    private(set) var hevc = false

    /// This Mac decodes HEVC in hardware (every Apple silicon Mac and recent Intel ones), so it
    /// can offer HEVC as a viewer without costing delay.
    static var hardwareDecodesHEVC: Bool { VTIsHardwareDecodeSupported(kCMVideoCodecType_HEVC) }

    var dimensions: CMVideoDimensions? {
        format.map { CMVideoFormatDescriptionGetDimensions($0) }
    }

    /// Returns nil until a keyframe with parameter sets has been seen.
    mutating func sampleBuffer(from data: Data) -> CMSampleBuffer? {
        let bytes = [UInt8](data)
        let nals = AnnexB.nalUnits(bytes)
        // A keyframe of the other codec: start over with its parameter sets.
        let switchTo: Bool? = nals.contains(where: AnnexB.isHevcVPS) ? true : nals.contains(where: AnnexB.isH264SPS) ? false : nil
        if let switchTo, switchTo != hevc {
            hevc = switchTo
            (vps, sps, pps, format) = (nil, nil, nil, nil)
        }
        var paramsChanged = false
        for nal in nals {
            let set: Int? = hevc ? [32: 0, 33: 1, 34: 2][AnnexB.hevcNalType(nal)] : [7: 1, 8: 2][AnnexB.nalType(nal)]
            switch set {
            case 0: if vps != Array(nal) { vps = Array(nal); paramsChanged = true }
            case 1: if sps != Array(nal) { sps = Array(nal); paramsChanged = true }
            case 2: if pps != Array(nal) { pps = Array(nal); paramsChanged = true }
            default: break
            }
        }
        if paramsChanged || format == nil, let sps, let pps {
            if hevc {
                if let vps { format = Self.makeHEVCFormat(vps: vps, sps: sps, pps: pps) }
            } else {
                format = Self.makeFormat(sps: sps, pps: pps)
            }
        }
        guard let format else { return nil }
        let avcc = AnnexB.toAVCC(nals, hevc: hevc)
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

    static func makeHEVCFormat(vps: [UInt8], sps: [UInt8], pps: [UInt8]) -> CMVideoFormatDescription? {
        var format: CMVideoFormatDescription?
        let status = vps.withUnsafeBufferPointer { v in
            sps.withUnsafeBufferPointer { s in
                pps.withUnsafeBufferPointer { p in
                    let pointers = [v.baseAddress!, s.baseAddress!, p.baseAddress!]
                    let sizes = [v.count, s.count, p.count]
                    return CMVideoFormatDescriptionCreateFromHEVCParameterSets(allocator: kCFAllocatorDefault, parameterSetCount: 3,
                                                                               parameterSetPointers: pointers, parameterSetSizes: sizes,
                                                                               nalUnitHeaderLength: 4, extensions: nil,
                                                                               formatDescriptionOut: &format)
                }
            }
        }
        return status == noErr ? format : nil
    }
}
