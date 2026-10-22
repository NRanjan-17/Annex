//! Pure helpers shared by the renderer (letterboxing, NV12 plane copies).

/// A rectangle in pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

/// Largest rectangle with the source aspect ratio that fits in `dst`, centred (letterbox /
/// pillarbox).
pub fn fit_rect(src_w: u32, src_h: u32, dst_w: u32, dst_h: u32) -> Rect {
    if src_w == 0 || src_h == 0 || dst_w == 0 || dst_h == 0 {
        return Rect { x: 0, y: 0, w: dst_w as i32, h: dst_h as i32 };
    }
    let (sw, sh, dw, dh) = (src_w as u64, src_h as u64, dst_w as u64, dst_h as u64);
    let (w, h) = if sw * dh > dw * sh { (dw, dw * sh / sw) } else { (dh * sw / sh, dh) };
    Rect { x: ((dw - w) / 2) as i32, y: ((dh - h) / 2) as i32, w: w as i32, h: h as i32 }
}

/// Copies an NV12 image (`height` luma rows plus `height / 2` interleaved chroma rows) between
/// buffers with different row pitches. `src_uv_offset`/`dst_uv_offset` locate the chroma plane.
#[allow(clippy::too_many_arguments)]
pub fn copy_nv12(
    src: &[u8],
    src_pitch: usize,
    src_uv_offset: usize,
    dst: &mut [u8],
    dst_pitch: usize,
    dst_uv_offset: usize,
    width: usize,
    height: usize,
) -> bool {
    let rows = |pitch: usize, uv: usize| uv + pitch * (height / 2);
    if width > src_pitch
        || width > dst_pitch
        || src.len() < rows(src_pitch, src_uv_offset)
        || dst.len() < rows(dst_pitch, dst_uv_offset)
    {
        return false;
    }
    for y in 0..height {
        dst[y * dst_pitch..y * dst_pitch + width].copy_from_slice(&src[y * src_pitch..y * src_pitch + width]);
    }
    for y in 0..height / 2 {
        let s = src_uv_offset + y * src_pitch;
        let d = dst_uv_offset + y * dst_pitch;
        dst[d..d + width].copy_from_slice(&src[s..s + width]);
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn letterbox_and_pillarbox() {
        assert_eq!(fit_rect(1920, 1080, 1920, 1200), Rect { x: 0, y: 60, w: 1920, h: 1080 });
        assert_eq!(fit_rect(1440, 900, 1920, 1080), Rect { x: 96, y: 0, w: 1728, h: 1080 });
        assert_eq!(fit_rect(1920, 1080, 3840, 2160), Rect { x: 0, y: 0, w: 3840, h: 2160 });
        assert_eq!(fit_rect(0, 0, 100, 50), Rect { x: 0, y: 0, w: 100, h: 50 });
    }

    #[test]
    fn nv12_copy_respects_pitch() {
        let (w, h) = (4, 4);
        // Source: pitch 4, chroma right after luma. Destination: pitch 8, chroma at row 6.
        let src: Vec<u8> = (0..(w * h + w * h / 2) as u8).collect();
        let mut dst = vec![0u8; 8 * 6 + 8 * 2];
        assert!(copy_nv12(&src, 4, 16, &mut dst, 8, 48, w, h));
        assert_eq!(&dst[0..4], &[0, 1, 2, 3]);
        assert_eq!(&dst[8..12], &[4, 5, 6, 7]);
        assert_eq!(&dst[48..52], &[16, 17, 18, 19]);
        assert_eq!(&dst[56..60], &[20, 21, 22, 23]);
        assert!(!copy_nv12(&src, 4, 16, &mut dst[..10], 8, 48, w, h));
    }
}
