//! Video over unreliable datagrams (QUIC, M5): each encoded frame is split into equal-size
//! shards plus Reed-Solomon parity shards, so a frame survives the loss of up to `parity`
//! of its datagrams without any retransmission.
//!
//! ```text
//! +-----------+--------------+-------+-----------+-------------+--------------+-------------+-----------+
//! | frame_id:4| capture_us:8 | flags:1| reserved:1| shard_idx:2 | data_count:2 | parity_cnt:2 | frame_len:4|  shard ...
//! +-----------+--------------+-------+-----------+-------------+--------------+-------------+-----------+
//! ```
//! Big-endian, 24-byte header. Shards `0..data_count` carry the frame (last one zero-padded to
//! the common shard size); shards `data_count..` are parity. Every shard of a frame has the same
//! length (required by Reed-Solomon).

use std::collections::BTreeMap;

use crate::{ProtoError, VideoFlags};

pub const SHARD_HEADER_LEN: usize = 24;
/// Upper bound on shards per frame (data + parity).
pub const MAX_SHARDS: usize = 4096;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShardHeader {
    pub frame_id: u32,
    pub capture_ts_us: i64,
    pub flags: VideoFlags,
    pub shard_index: u16,
    pub data_count: u16,
    pub parity_count: u16,
    pub frame_len: u32,
}

impl ShardHeader {
    pub fn to_bytes(self) -> [u8; SHARD_HEADER_LEN] {
        let mut b = [0u8; SHARD_HEADER_LEN];
        b[0..4].copy_from_slice(&self.frame_id.to_be_bytes());
        b[4..12].copy_from_slice(&self.capture_ts_us.to_be_bytes());
        b[12] = self.flags.0;
        b[14..16].copy_from_slice(&self.shard_index.to_be_bytes());
        b[16..18].copy_from_slice(&self.data_count.to_be_bytes());
        b[18..20].copy_from_slice(&self.parity_count.to_be_bytes());
        b[20..24].copy_from_slice(&self.frame_len.to_be_bytes());
        b
    }

    pub fn parse(b: &[u8]) -> Result<Self, ProtoError> {
        let b = b.first_chunk::<SHARD_HEADER_LEN>().ok_or(ProtoError::Truncated)?;
        let h = ShardHeader {
            frame_id: u32::from_be_bytes(b[0..4].try_into().unwrap()),
            capture_ts_us: i64::from_be_bytes(b[4..12].try_into().unwrap()),
            flags: VideoFlags(b[12]),
            shard_index: u16::from_be_bytes(b[14..16].try_into().unwrap()),
            data_count: u16::from_be_bytes(b[16..18].try_into().unwrap()),
            parity_count: u16::from_be_bytes(b[18..20].try_into().unwrap()),
            frame_len: u32::from_be_bytes(b[20..24].try_into().unwrap()),
        };
        let total = h.data_count as usize + h.parity_count as usize;
        if h.data_count == 0 || total > MAX_SHARDS || h.shard_index as usize >= total {
            return Err(ProtoError::InvalidField("shard"));
        }
        Ok(h)
    }
}

/// Parity shards for `data` data shards at `fec_percent` overhead (at least one).
pub fn parity_for(data: usize, fec_percent: u32) -> usize {
    (data * fec_percent as usize).div_ceil(100).max(1)
}

/// Splits one frame into datagrams no larger than `max_datagram` bytes.
pub fn fragment(
    frame_id: u32,
    capture_ts_us: i64,
    flags: VideoFlags,
    data: &[u8],
    max_datagram: usize,
    fec_percent: u32,
) -> Result<Vec<Vec<u8>>, ProtoError> {
    if max_datagram <= SHARD_HEADER_LEN + 2 {
        return Err(ProtoError::InvalidField("max_datagram"));
    }
    // Reed-Solomon needs an even shard size.
    let max_shard = (max_datagram - SHARD_HEADER_LEN) & !1;
    let data_count = data.len().max(1).div_ceil(max_shard);
    let shard_len = data.len().max(1).div_ceil(data_count).next_multiple_of(2);
    let parity_count = parity_for(data_count, fec_percent);
    if data_count + parity_count > MAX_SHARDS {
        return Err(ProtoError::RecordTooLarge(data.len()));
    }
    let mut originals: Vec<Vec<u8>> = (0..data_count)
        .map(|i| {
            let start = (i * shard_len).min(data.len());
            let end = ((i + 1) * shard_len).min(data.len());
            let mut s = data[start..end].to_vec();
            s.resize(shard_len, 0);
            s
        })
        .collect();
    let parity = reed_solomon_simd::encode(data_count, parity_count, &originals)
        .map_err(|e| ProtoError::Malformed(format!("fec: {e}")))?;
    originals.extend(parity);
    Ok(originals
        .into_iter()
        .enumerate()
        .map(|(i, shard)| {
            let header = ShardHeader {
                frame_id,
                capture_ts_us,
                flags,
                shard_index: i as u16,
                data_count: data_count as u16,
                parity_count: parity_count as u16,
                frame_len: data.len() as u32,
            };
            let mut d = Vec::with_capacity(SHARD_HEADER_LEN + shard.len());
            d.extend_from_slice(&header.to_bytes());
            d.extend_from_slice(&shard);
            d
        })
        .collect())
}

/// A frame rebuilt from its shards.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reassembled {
    pub frame_id: u32,
    pub capture_ts_us: i64,
    pub flags: VideoFlags,
    pub data: Vec<u8>,
    /// Shards that were missing and rebuilt from parity.
    pub recovered: usize,
}

struct Pending {
    header: ShardHeader,
    shards: BTreeMap<u16, Vec<u8>>,
    shard_len: usize,
}

/// Collects shards and emits frames in order. A frame that can't be completed before a newer
/// frame finishes is declared lost (the viewer then asks for a keyframe).
#[derive(Default)]
pub struct Reassembler {
    pending: BTreeMap<u32, Pending>,
    /// Frames at or below this id are done (delivered or lost).
    done_through: Option<u32>,
    pub frames_lost: u64,
    pub shards_received: u64,
    pub shards_recovered: u64,
}

/// What [`Reassembler::push`] produced.
#[derive(Debug, Default)]
pub struct PushResult {
    pub frames: Vec<Reassembled>,
    /// Frames given up on (older than a completed frame).
    pub lost: Vec<u32>,
}

impl Reassembler {
    pub fn push(&mut self, datagram: &[u8]) -> Result<PushResult, ProtoError> {
        let header = ShardHeader::parse(datagram)?;
        let shard = &datagram[SHARD_HEADER_LEN..];
        let mut out = PushResult::default();
        if self.done_through.is_some_and(|d| header.frame_id <= d) {
            return Ok(out); // late shard of a finished frame
        }
        self.shards_received += 1;
        let entry = self.pending.entry(header.frame_id).or_insert_with(|| Pending {
            header,
            shards: BTreeMap::new(),
            shard_len: shard.len(),
        });
        if shard.len() != entry.shard_len || header.data_count != entry.header.data_count {
            return Err(ProtoError::InvalidField("shard length"));
        }
        entry.shards.insert(header.shard_index, shard.to_vec());
        if entry.shards.len() < entry.header.data_count as usize {
            return Ok(out);
        }
        let done = self.pending.remove(&header.frame_id).unwrap();
        // Everything older than a completed frame is lost.
        let older: Vec<u32> = self.pending.range(..header.frame_id).map(|(id, _)| *id).collect();
        for id in older {
            self.pending.remove(&id);
            self.frames_lost += 1;
            out.lost.push(id);
        }
        self.done_through = Some(header.frame_id);
        out.frames.push(self.rebuild(done)?);
        Ok(out)
    }

    fn rebuild(&mut self, p: Pending) -> Result<Reassembled, ProtoError> {
        let h = p.header;
        let data_count = h.data_count as usize;
        let missing: Vec<usize> = (0..data_count).filter(|i| !p.shards.contains_key(&(*i as u16))).collect();
        let mut originals: Vec<Option<Vec<u8>>> = (0..data_count).map(|i| p.shards.get(&(i as u16)).cloned()).collect();
        if !missing.is_empty() {
            let present = p.shards.iter().filter(|(i, _)| (**i as usize) < data_count).map(|(i, s)| (*i as usize, s));
            let parity = p
                .shards
                .iter()
                .filter(|(i, _)| (**i as usize) >= data_count)
                .map(|(i, s)| (*i as usize - data_count, s));
            let restored = reed_solomon_simd::decode(data_count, h.parity_count as usize, present, parity)
                .map_err(|e| ProtoError::Malformed(format!("fec: {e}")))?;
            for (i, s) in restored {
                originals[i] = Some(s);
            }
            self.shards_recovered += missing.len() as u64;
        }
        let mut data = Vec::with_capacity(data_count * p.shard_len);
        for s in originals {
            data.extend_from_slice(&s.ok_or(ProtoError::Truncated)?);
        }
        data.truncate(h.frame_len as usize);
        Ok(Reassembled {
            frame_id: h.frame_id,
            capture_ts_us: h.capture_ts_us,
            flags: h.flags,
            data,
            recovered: missing.len(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(n: usize) -> Vec<u8> {
        (0..n).map(|i| (i * 31 % 251) as u8).collect()
    }

    #[test]
    fn header_roundtrip_and_validation() {
        let h = ShardHeader {
            frame_id: 7,
            capture_ts_us: -3,
            flags: VideoFlags(1),
            shard_index: 2,
            data_count: 3,
            parity_count: 1,
            frame_len: 1000,
        };
        assert_eq!(ShardHeader::parse(&h.to_bytes()).unwrap(), h);
        assert!(ShardHeader::parse(&ShardHeader { shard_index: 4, ..h }.to_bytes()).is_err());
        assert!(ShardHeader::parse(&ShardHeader { data_count: 0, ..h }.to_bytes()).is_err());
        assert_eq!(ShardHeader::parse(&[0; 10]), Err(ProtoError::Truncated));
    }

    #[test]
    fn datagrams_fit_and_reassemble_without_loss() {
        let data = frame(10_000);
        let grams = fragment(1, 99, VideoFlags(1), &data, 1200, 20).unwrap();
        assert!(grams.iter().all(|g| g.len() <= 1200));
        assert_eq!(grams.len(), 9 + 2); // 9 data shards, 20 % parity rounded up
        let mut r = Reassembler::default();
        let mut got = Vec::new();
        for g in &grams {
            got.extend(r.push(g).unwrap().frames);
        }
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].data, data);
        assert_eq!((got[0].frame_id, got[0].capture_ts_us, got[0].recovered), (1, 99, 0));
    }

    #[test]
    fn lost_shards_are_rebuilt_from_parity() {
        let data = frame(50_000);
        let grams = fragment(5, 0, VideoFlags(0), &data, 1200, 25).unwrap();
        let parity = grams.len() - 50_000usize.div_ceil(1176);
        // Drop as many data shards as there are parity shards, spread out.
        let drop: Vec<usize> = (0..parity).map(|i| i * 3).collect();
        let mut r = Reassembler::default();
        let mut got = Vec::new();
        for (i, g) in grams.iter().enumerate() {
            if !drop.contains(&i) {
                got.extend(r.push(g).unwrap().frames);
            }
        }
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].data, data);
        assert_eq!(got[0].recovered, parity);
    }

    #[test]
    fn too_much_loss_means_the_frame_is_lost_when_a_newer_one_completes() {
        let mut r = Reassembler::default();
        let a = fragment(1, 0, VideoFlags(0), &frame(5000), 1200, 10).unwrap();
        let b = fragment(2, 0, VideoFlags(0), &frame(500), 1200, 10).unwrap();
        for g in &a[2..] {
            assert!(r.push(g).unwrap().frames.is_empty());
        }
        let mut out = PushResult::default();
        for g in &b {
            let p = r.push(g).unwrap();
            out.frames.extend(p.frames);
            out.lost.extend(p.lost);
        }
        assert_eq!(out.lost, vec![1]);
        assert_eq!(out.frames.len(), 1);
        assert_eq!(r.frames_lost, 1);
        // Late shards of the lost frame are ignored.
        assert!(r.push(&a[0]).unwrap().frames.is_empty());
    }

    #[test]
    fn tiny_and_empty_frames() {
        for n in [0, 1, 2, 3] {
            let data = frame(n);
            let grams = fragment(9, 0, VideoFlags(0), &data, 100, 0).unwrap();
            let mut r = Reassembler::default();
            let got: Vec<_> = grams.iter().flat_map(|g| r.push(g).unwrap().frames).collect();
            assert_eq!(got[0].data, data);
        }
    }

    #[test]
    fn rejects_oversized_frames_and_tiny_datagrams() {
        assert!(fragment(1, 0, VideoFlags(0), &frame(10), SHARD_HEADER_LEN, 10).is_err());
        assert!(fragment(1, 0, VideoFlags(0), &frame(20_000_000), 1200, 10).is_err());
    }
}
