//! Sound over unreliable datagrams, beside video. One Opus packet (20 ms) per datagram:
//!
//! ```text
//! +-------+--------------+--------+-----------+-------------+
//! | seq:4 | capture_us:8 | kind:1 | marker:1  | opus packet |
//! +-------+--------------+--------+-----------+-------------+
//! ```
//! Big-endian. Byte 13 is reserved (always 0) in video shards, so `marker` tells the two apart.
//! Sent only to peers whose hello lists the `audio` capability.

use crate::ProtoError;

pub const HEADER_LEN: usize = 14;
const MARKER: u8 = 0xA5;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioKind {
    /// What the shared computer is playing (host → viewer).
    Sound,
    /// The viewer's microphone (viewer → host).
    Microphone,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioHeader {
    pub seq: u32,
    pub capture_ts_us: i64,
    pub kind: AudioKind,
}

/// Whether a datagram carries sound rather than a video shard.
pub fn is_audio(datagram: &[u8]) -> bool {
    datagram.len() > HEADER_LEN && datagram[13] == MARKER
}

impl AudioHeader {
    /// The header followed by `packet`.
    pub fn with(self, packet: &[u8]) -> Vec<u8> {
        let mut b = Vec::with_capacity(HEADER_LEN + packet.len());
        b.extend_from_slice(&self.seq.to_be_bytes());
        b.extend_from_slice(&self.capture_ts_us.to_be_bytes());
        b.push(match self.kind {
            AudioKind::Sound => 0,
            AudioKind::Microphone => 1,
        });
        b.push(MARKER);
        b.extend_from_slice(packet);
        b
    }

    /// The header and the Opus packet after it.
    pub fn parse(b: &[u8]) -> Result<(AudioHeader, &[u8]), ProtoError> {
        if !is_audio(b) {
            return Err(ProtoError::Truncated);
        }
        let kind = match b[12] {
            0 => AudioKind::Sound,
            1 => AudioKind::Microphone,
            _ => return Err(ProtoError::InvalidField("audio kind")),
        };
        let h = AudioHeader {
            seq: u32::from_be_bytes(b[0..4].try_into().unwrap()),
            capture_ts_us: i64::from_be_bytes(b[4..12].try_into().unwrap()),
            kind,
        };
        Ok((h, &b[HEADER_LEN..]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fragment::{ShardHeader, fragment};

    #[test]
    fn round_trips_and_is_told_apart_from_video() {
        let h = AudioHeader { seq: 7, capture_ts_us: -3, kind: AudioKind::Microphone };
        let d = h.with(&[1, 2, 3]);
        assert!(is_audio(&d));
        assert_eq!(AudioHeader::parse(&d).unwrap(), (h, &[1u8, 2, 3][..]));
        for shard in fragment(9, 1, crate::VideoFlags(1), &[5u8; 5000], 1200, 20).unwrap() {
            assert!(!is_audio(&shard), "a video shard isn't sound");
            ShardHeader::parse(&shard).unwrap();
        }
        assert!(AudioHeader::parse(&[0; HEADER_LEN]).is_err(), "no packet");
    }
}
