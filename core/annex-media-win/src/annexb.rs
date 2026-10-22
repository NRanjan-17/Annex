//! Platform-independent H.264 Annex B helpers used by the Windows encoder.

/// NAL unit types present in an Annex B buffer.
pub fn nal_types(buf: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut i = 0;
    while i + 3 < buf.len() {
        if buf[i] == 0 && buf[i + 1] == 0 && buf[i + 2] == 1 {
            out.push(buf[i + 3] & 0x1f);
            i += 3;
        } else {
            i += 1;
        }
    }
    out
}

/// Ensures a keyframe carries SPS/PPS: if `au` has none, prepends `sequence_header` (Annex B
/// SPS+PPS from the encoder's output type).
pub fn with_parameter_sets(au: Vec<u8>, sequence_header: &[u8]) -> Vec<u8> {
    let types = nal_types(&au);
    if types.contains(&7) || sequence_header.is_empty() {
        return au;
    }
    let mut out = Vec::with_capacity(sequence_header.len() + au.len());
    out.extend_from_slice(sequence_header);
    out.extend_from_slice(&au);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_nal_types() {
        assert_eq!(nal_types(&[0, 0, 0, 1, 0x67, 1, 0, 0, 1, 0x68, 2, 0, 0, 1, 0x65]), vec![7, 8, 5]);
    }

    #[test]
    fn prepends_only_when_missing() {
        let sps_pps = vec![0, 0, 0, 1, 0x67, 9, 0, 0, 0, 1, 0x68, 9];
        let idr = vec![0, 0, 0, 1, 0x65, 1];
        assert_eq!(nal_types(&with_parameter_sets(idr.clone(), &sps_pps)), vec![7, 8, 5]);
        let full = [sps_pps.clone(), idr].concat();
        assert_eq!(with_parameter_sets(full.clone(), &sps_pps), full);
    }
}
