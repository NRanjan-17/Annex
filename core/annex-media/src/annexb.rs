//! Platform-independent H.264 and HEVC Annex B helpers used by the hosts' encoders.

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

/// Whether an Annex B access unit carries an HEVC VPS (type 32, layer 0). Only HEVC keyframes do,
/// so it tells the two codecs apart when a stream switches.
pub fn has_hevc_vps(buf: &[u8]) -> bool {
    buf.windows(5).any(|w| w[..3] == [0, 0, 1] && w[3] == 0x40 && w[4] == 0x01)
}

/// Ensures an HEVC keyframe carries VPS/SPS/PPS: if `au` has no VPS, prepends `sequence_header`.
pub fn with_hevc_parameter_sets(au: Vec<u8>, sequence_header: &[u8]) -> Vec<u8> {
    if has_hevc_vps(&au) || sequence_header.is_empty() {
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

    #[test]
    fn hevc_keyframes_get_their_parameter_sets() {
        // VPS (0x40 0x01), SPS (0x42 0x01), PPS (0x44 0x01), then an IDR slice (type 19: 0x26 0x01).
        let vps_sps_pps = vec![0, 0, 0, 1, 0x40, 1, 9, 0, 0, 0, 1, 0x42, 1, 9, 0, 0, 0, 1, 0x44, 1, 9];
        let idr = vec![0, 0, 0, 1, 0x26, 1, 7];
        assert!(!has_hevc_vps(&idr));
        let key = with_hevc_parameter_sets(idr.clone(), &vps_sps_pps);
        assert!(has_hevc_vps(&key));
        assert_eq!(with_hevc_parameter_sets(key.clone(), &vps_sps_pps), key);
        // An H.264 SPS is never mistaken for a VPS.
        assert!(!has_hevc_vps(&[0, 0, 0, 1, 0x67, 0x64, 0, 0x1f]));
    }
}
