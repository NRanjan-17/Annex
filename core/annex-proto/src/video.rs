//! Video record payload: a fixed 16-byte header followed by one encoded access unit.
//!
//! ```text
//! +-------------+--------------------+---------+-------------+--------------+
//! | frame_id:4  | capture_ts_us:8    | flags:1 | reserved:3  | data ...     |
//! +-------------+--------------------+---------+-------------+--------------+
//! ```
//! All integers are big-endian. `capture_ts_us` is on the host's monotonic clock;
//! viewers convert it with the offset estimated from `ClockPing`/`ClockPong`.

use crate::ProtoError;

pub const VIDEO_HEADER_LEN: usize = 16;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct VideoFlags(pub u8);

impl VideoFlags {
    pub const KEYFRAME: u8 = 0x01;

    pub fn keyframe(self) -> bool {
        self.0 & Self::KEYFRAME != 0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VideoHeader {
    pub frame_id: u32,
    pub capture_ts_us: i64,
    pub flags: VideoFlags,
}

impl VideoHeader {
    pub fn to_bytes(self) -> [u8; VIDEO_HEADER_LEN] {
        let mut b = [0u8; VIDEO_HEADER_LEN];
        b[0..4].copy_from_slice(&self.frame_id.to_be_bytes());
        b[4..12].copy_from_slice(&self.capture_ts_us.to_be_bytes());
        b[12] = self.flags.0;
        b
    }

    pub fn parse(b: &[u8]) -> Result<Self, ProtoError> {
        let b = b.first_chunk::<VIDEO_HEADER_LEN>().ok_or(ProtoError::Truncated)?;
        Ok(VideoHeader {
            frame_id: u32::from_be_bytes(b[0..4].try_into().unwrap()),
            capture_ts_us: i64::from_be_bytes(b[4..12].try_into().unwrap()),
            flags: VideoFlags(b[12]),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VideoFrame {
    pub header: VideoHeader,
    pub data: Vec<u8>,
}

impl VideoFrame {
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(VIDEO_HEADER_LEN + self.data.len());
        out.extend_from_slice(&self.header.to_bytes());
        out.extend_from_slice(&self.data);
        out
    }

    pub fn decode(payload: &[u8]) -> Result<Self, ProtoError> {
        let header = VideoHeader::parse(payload)?;
        Ok(VideoFrame { header, data: payload[VIDEO_HEADER_LEN..].to_vec() })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let f = VideoFrame {
            header: VideoHeader { frame_id: 0xdead_beef, capture_ts_us: -42, flags: VideoFlags(VideoFlags::KEYFRAME) },
            data: vec![0, 0, 0, 1, 0x65],
        };
        let back = VideoFrame::decode(&f.encode()).unwrap();
        assert_eq!(back, f);
        assert!(back.header.flags.keyframe());
    }

    #[test]
    fn truncated() {
        assert_eq!(VideoFrame::decode(&[0; 15]), Err(ProtoError::Truncated));
    }
}
