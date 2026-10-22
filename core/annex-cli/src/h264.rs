//! Minimal H.264 Annex B helpers: split a byte stream into access units (for replaying a
//! recorded stream) and detect keyframes.

/// Iterates NAL units (without start codes) in an Annex B buffer.
pub fn nal_units(buf: &[u8]) -> Vec<&[u8]> {
    let mut starts = Vec::new();
    let mut i = 0;
    while i + 3 <= buf.len() {
        if buf[i] == 0 && buf[i + 1] == 0 && buf[i + 2] == 1 {
            starts.push(i + 3);
            i += 3;
        } else {
            i += 1;
        }
    }
    let mut out = Vec::with_capacity(starts.len());
    for (k, &s) in starts.iter().enumerate() {
        let mut end = starts.get(k + 1).map(|n| n - 3).unwrap_or(buf.len());
        // A 4-byte start code leaves one trailing zero on the previous NAL.
        while end > s && buf[end - 1] == 0 && k + 1 < starts.len() {
            end -= 1;
        }
        if end > s {
            out.push(&buf[s..end]);
        }
    }
    out
}

pub fn nal_type(nal: &[u8]) -> u8 {
    nal.first().map(|b| b & 0x1f).unwrap_or(0)
}

/// True if the access unit contains an IDR slice.
pub fn is_keyframe(au: &[u8]) -> bool {
    nal_units(au).iter().any(|n| nal_type(n) == 5)
}

/// Splits an Annex B stream into access units. A new AU starts at an access unit delimiter,
/// or at a slice whose `first_mb_in_slice` is 0 when no delimiter preceded it. Parameter sets
/// and SEI attach to the following picture.
pub fn split_access_units(buf: &[u8]) -> Vec<Vec<u8>> {
    let mut aus: Vec<Vec<u8>> = Vec::new();
    let mut cur: Vec<u8> = Vec::new();
    let mut cur_has_slice = false;
    for nal in nal_units(buf) {
        let t = nal_type(nal);
        let first_slice = matches!(t, 1 | 5) && nal.get(1).is_some_and(|b| b & 0x80 != 0);
        let starts_new = cur_has_slice && (t == 9 || matches!(t, 6..=8) || first_slice);
        if starts_new {
            aus.push(std::mem::take(&mut cur));
            cur_has_slice = false;
        }
        cur.extend_from_slice(&[0, 0, 0, 1]);
        cur.extend_from_slice(nal);
        if matches!(t, 1 | 5) {
            cur_has_slice = true;
        }
    }
    if cur_has_slice {
        aus.push(cur);
    }
    aus
}

#[cfg(test)]
mod tests {
    use super::*;

    const SPS: &[u8] = &[0, 0, 0, 1, 0x67, 0x42];
    const PPS: &[u8] = &[0, 0, 0, 1, 0x68, 0xce];
    const IDR: &[u8] = &[0, 0, 1, 0x65, 0x88, 0x01];
    const P1: &[u8] = &[0, 0, 0, 1, 0x41, 0x9a, 0x02];
    const P1_SLICE2: &[u8] = &[0, 0, 1, 0x41, 0x1a, 0x03];

    #[test]
    fn splits_and_detects_keyframes() {
        let stream = [SPS, PPS, IDR, P1, P1_SLICE2, P1].concat();
        let aus = split_access_units(&stream);
        assert_eq!(aus.len(), 3);
        assert!(is_keyframe(&aus[0]));
        assert_eq!(nal_units(&aus[0]).len(), 3);
        assert!(!is_keyframe(&aus[1]));
        assert_eq!(nal_units(&aus[1]).len(), 2, "second slice stays in the same picture");
    }

    #[test]
    fn nal_units_handle_mixed_start_codes() {
        let n = nal_units(&[0, 0, 0, 1, 0x67, 1, 0, 0, 1, 0x68, 2]);
        assert_eq!(n, vec![&[0x67, 1][..], &[0x68, 2][..]]);
        assert!(nal_units(&[1, 2, 3]).is_empty());
    }
}
