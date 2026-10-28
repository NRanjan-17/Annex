//! Adaptive bitrate: turns viewer receiver reports into an encoder bitrate scale and an FEC level.
//!
//! Loss-based with a delay guard, in the spirit of GCC/TFRC but deliberately simple:
//! - a queue of a few hundred milliseconds (RTT ≥ 3 × the best + 100 ms) → halve at once: the
//!   link is far slower than the stream, and every second at this rate adds more delay;
//! - a lost frame, heavy datagram loss (> 5 %), or RTT rising well above the best seen →
//!   multiplicative decrease (× 0.8) and more parity;
//! - RTT creeping up (≥ 1.5 × the best + 8 ms, the first sign of a Wi-Fi queue building) →
//!   gentle decrease (× 0.9) before anything is lost;
//! - moderate datagram loss (2–5 %) → hold the rate, add parity;
//! - clean intervals → 5 % more each time (at least two points) and slowly less parity. Growing
//!   by a share of the current rate, not of the nominal one, keeps a slow link from being
//!   overshot again within a second of recovering.
//!
//! The scale is a percentage of the app's normal bitrate for the stream, so the core needs no
//! codec knowledge.

use std::collections::VecDeque;

use annex_proto::{Mode, Quality, ReceiverReport, StreamConfig};

/// The floor, as a percentage of the nominal bitrate: about 1 Mbit/s for 1440p at 60 fps. A
/// link slower than the floor builds a queue that delays the picture by seconds, so it's low.
pub const MIN_SCALE: u32 = 4;
pub const MAX_SCALE: u32 = 150;
pub const MIN_FEC: u32 = 10;
pub const MAX_FEC: u32 = 40;
/// The "best" RTT is the lowest of this many recent reports (about 10 s), so one lucky sample
/// doesn't make a steady, higher RTT look congested for the rest of the session.
const RTT_WINDOW: usize = 10;

#[derive(Debug, Clone)]
pub struct RateController {
    scale: u32,
    fec: u32,
    /// Recent non-zero RTTs, newest last.
    recent_rtt_us: VecDeque<u32>,
    /// Highest scale the viewer's quality choice allows.
    ceiling: u32,
}

impl Default for RateController {
    fn default() -> Self {
        RateController::new(Quality::Best)
    }
}

/// Bitrate ceiling (percent of the normal rate) for a quality choice.
pub fn quality_ceiling(q: Quality) -> u32 {
    match q {
        Quality::Best => MAX_SCALE,
        Quality::Balanced => 85,
        Quality::Fast => 55,
    }
}

/// A bitrate cap for a relay, about 6 Mbit/s ([`relay_ceiling`]). Sessions on our relay aren't
/// held to it any more: the relay is only the fallback, with a daily login limit per device.
pub const RELAY_BPS: u64 = 6_000_000;

/// The apps' normal bitrate for a stream (macOS `HostController` and `ExtendSession`, and their
/// Windows twins, use the same formulas).
pub fn nominal_bps(mode: Mode, s: &StreamConfig) -> u64 {
    let (w, h, fps) = (u64::from(s.width), u64::from(s.height), u64::from(s.fps));
    match mode {
        Mode::Mirror => (w * h * fps / 10).max(4_000_000),
        Mode::Extend => (w * h * (fps.min(60) + fps.saturating_sub(60) / 2) / 8).max(6_000_000),
    }
}

/// The ceiling on our relay: [`RELAY_BPS`], and never above Fast.
pub fn relay_ceiling(mode: Mode, s: &StreamConfig) -> u32 {
    let percent = RELAY_BPS * 100 / nominal_bps(mode, s).max(1);
    (percent as u32).clamp(1, quality_ceiling(Quality::Fast))
}

/// Below this many bits a second a full-size mirror picture smears text, so the host sends a
/// smaller one ([`mirror_picture`]).
pub const SMALLER_PICTURE_BELOW_BPS: u64 = 2_500_000;
/// The full picture comes back above this. The gap keeps a link near one line from switching
/// back and forth (each switch costs a keyframe).
pub const FULL_PICTURE_ABOVE_BPS: u64 = 4_500_000;
/// The smaller picture is never under 960×540's worth of pixels. A stream that small already
/// has a low nominal bitrate (Quality Fast keeps 720p30 at 2.2 Mbit/s on a perfect link), so
/// halving it would only blur it.
const MIN_SMALLER_PIXELS: u64 = 960 * 540;
/// The smaller picture's frame rate cap.
const SMALLER_PICTURE_FPS: u32 = 30;

/// Whether the host should send the smaller picture at `effective_bps` (the encoder's current
/// target: its normal bitrate × the scale), with hysteresis around [`SMALLER_PICTURE_BELOW_BPS`]
/// and [`FULL_PICTURE_ABOVE_BPS`]. `currently_reduced` is what it sends now.
pub fn reduced_picture(effective_bps: u64, currently_reduced: bool) -> bool {
    if currently_reduced { effective_bps <= FULL_PICTURE_ABOVE_BPS } else { effective_bps < SMALLER_PICTURE_BELOW_BPS }
}

/// The smaller picture for a stream: each side halved (rounded down to even) and at most 30
/// fps. None if the stream is already too small to halve.
pub fn reduced_stream(full: &StreamConfig) -> Option<StreamConfig> {
    let (w, h) = ((full.width / 2) & !1, (full.height / 2) & !1);
    if u64::from(w) * u64::from(h) < MIN_SMALLER_PIXELS {
        return None;
    }
    Some(StreamConfig { codec: full.codec, width: w, height: h, fps: full.fps.min(SMALLER_PICTURE_FPS) })
}

/// What a mirror host should encode: (smaller, picture). `full` is the session's stream,
/// `effective_bps` the encoder's current target and `currently_reduced` whether it sends the
/// smaller picture now. The hosts keep the bitrate as it is; only the picture changes.
pub fn mirror_picture(full: &StreamConfig, effective_bps: u64, currently_reduced: bool) -> (bool, StreamConfig) {
    match reduced_stream(full) {
        Some(small) if reduced_picture(effective_bps, currently_reduced) => (true, small),
        _ => (false, full.clone()),
    }
}

/// What changed after a report.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RateDecision {
    pub scale_percent: u32,
    pub fec_percent: u32,
    /// True if the bitrate scale changed (apps only need to act then).
    pub scale_changed: bool,
}

impl RateController {
    pub fn new(quality: Quality) -> Self {
        let ceiling = quality_ceiling(quality);
        RateController { scale: ceiling.min(100), fec: 15, recent_rtt_us: VecDeque::new(), ceiling }
    }

    /// Capped at `ceiling` percent (e.g. [`relay_ceiling`]), which may be below [`MIN_SCALE`].
    pub fn with_ceiling(ceiling: u32) -> Self {
        RateController { scale: ceiling.min(100), fec: 15, recent_rtt_us: VecDeque::new(), ceiling }
    }

    pub fn scale_percent(&self) -> u32 {
        self.scale
    }

    pub fn fec_percent(&self) -> u32 {
        self.fec
    }

    pub fn on_report(&mut self, r: &ReceiverReport) -> RateDecision {
        let before = self.scale;
        // Widened: the counts come from the peer and may sum past u32::MAX.
        let total = r.shards as u64 + r.recovered as u64;
        let shard_loss = if total == 0 { 0.0 } else { r.recovered as f64 / total as f64 };
        let best_rtt_us = self.recent_rtt_us.iter().copied().min();
        let (rtt_queued, rtt_congested, rtt_rising) = match (best_rtt_us, r.rtt_us) {
            (_, 0) | (None, _) => (false, false, false),
            (Some(best), rtt) => (
                rtt > best.saturating_mul(3).saturating_add(100_000),
                rtt > best.saturating_mul(2).saturating_add(15_000),
                rtt > best.saturating_add(best / 2).saturating_add(8_000),
            ),
        };
        if r.rtt_us > 0 {
            if self.recent_rtt_us.len() == RTT_WINDOW {
                self.recent_rtt_us.pop_front();
            }
            self.recent_rtt_us.push_back(r.rtt_us);
        }

        if rtt_queued {
            self.scale = (self.scale / 2).max(MIN_SCALE.min(self.ceiling));
            self.fec = (self.fec + 5).min(MAX_FEC);
        } else if r.lost > 0 || shard_loss > 0.05 || rtt_congested {
            self.scale = (self.scale * 4 / 5).max(MIN_SCALE.min(self.ceiling));
            self.fec = (self.fec + 5).min(MAX_FEC);
        } else if rtt_rising {
            self.scale = (self.scale * 9 / 10).max(MIN_SCALE.min(self.ceiling));
        } else if shard_loss > 0.02 {
            self.fec = (self.fec + 2).min(MAX_FEC);
        } else if r.frames > 0 {
            self.scale = (self.scale + (self.scale / 20).max(2)).min(self.ceiling);
            self.fec = self.fec.saturating_sub(1).max(MIN_FEC);
        }
        RateDecision { scale_percent: self.scale, fec_percent: self.fec, scale_changed: self.scale != before }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report(frames: u32, lost: u32, shards: u32, recovered: u32, rtt_us: u32) -> ReceiverReport {
        ReceiverReport { frames, lost, shards, recovered, rtt_us }
    }

    #[test]
    fn our_relay_holds_about_6_mbit() {
        let s = |width, height, fps| StreamConfig { codec: annex_proto::Codec::H264, width, height, fps };
        // 720p mirror is under the limit even at Fast.
        assert_eq!(relay_ceiling(Mode::Mirror, &s(1280, 720, 60)), 55);
        // 4K Extend at 60 Hz: ~62 Mbit/s normally, 9 % of that on the relay.
        let big = s(3840, 2160, 60);
        let c = relay_ceiling(Mode::Extend, &big);
        assert_eq!(c, 9);
        assert!(nominal_bps(Mode::Extend, &big) * u64::from(c) / 100 <= RELAY_BPS);
        // Loss can't push it back up to the usual floor.
        let mut r = RateController::with_ceiling(c);
        r.on_report(&report(10, 1, 100, 0, 0));
        assert!(r.scale_percent() <= c);
        for _ in 0..20 {
            r.on_report(&report(10, 0, 100, 0, 0));
        }
        assert_eq!(r.scale_percent(), c);
    }

    #[test]
    fn clean_link_ramps_up_to_the_cap() {
        let mut c = RateController::default();
        for _ in 0..20 {
            c.on_report(&report(60, 0, 1500, 0, 3000));
        }
        assert_eq!(c.scale_percent(), MAX_SCALE);
        assert_eq!(c.fec_percent(), MIN_FEC);
    }

    #[test]
    fn lost_frames_back_off_multiplicatively_with_more_fec() {
        let mut c = RateController::default();
        let d = c.on_report(&report(55, 2, 1400, 30, 3000));
        assert_eq!((d.scale_percent, d.fec_percent, d.scale_changed), (80, 20, true));
        for _ in 0..20 {
            c.on_report(&report(10, 5, 300, 60, 3000));
        }
        assert_eq!(c.scale_percent(), MIN_SCALE);
        assert_eq!(c.fec_percent(), MAX_FEC);
    }

    #[test]
    fn quality_caps_the_rate() {
        let mut c = RateController::new(Quality::Fast);
        assert_eq!(c.scale_percent(), 55);
        for _ in 0..20 {
            c.on_report(&report(60, 0, 1500, 0, 3000));
        }
        assert_eq!(c.scale_percent(), 55, "Fast never climbs above its ceiling");
        assert_eq!(RateController::new(Quality::Balanced).scale_percent(), 85);
        assert_eq!(RateController::new(Quality::Best).scale_percent(), 100);
    }

    #[test]
    fn a_link_slower_than_the_stream_drains_its_queue() {
        // The Mint laptop over Tailscale: 140 ms of path, then seconds of queue at 20 % of a
        // 1440p stream. Halving on a deep queue reaches a rate the link carries within a few
        // reports, far below the old 20 % floor.
        let mut c = RateController::default();
        for _ in 0..3 {
            c.on_report(&report(25, 0, 200, 0, 140_000));
        }
        for _ in 0..5 {
            c.on_report(&report(25, 0, 300, 0, 1_200_000));
        }
        assert!(c.scale_percent() <= 8, "{}", c.scale_percent());
        assert!(c.scale_percent() >= MIN_SCALE);
    }

    #[test]
    fn huge_peer_counts_do_not_overflow() {
        let mut c = RateController::default();
        let d = c.on_report(&report(u32::MAX, 0, u32::MAX, u32::MAX, u32::MAX));
        assert!(d.scale_changed, "half the datagrams rebuilt is heavy loss");
    }

    fn stream(width: u32, height: u32, fps: u32) -> StreamConfig {
        StreamConfig { codec: annex_proto::Codec::H264, width, height, fps }
    }

    #[test]
    fn small_streams_stay_whole() {
        // 720p30 at Quality Fast sits at 2.2 Mbit/s (the 4 Mbit/s floor × 55 %) on a perfect
        // link: never halve that.
        let s = stream(1280, 720, 30);
        let fast = nominal_bps(Mode::Mirror, &s) * u64::from(quality_ceiling(Quality::Fast)) / 100;
        assert!(fast < SMALLER_PICTURE_BELOW_BPS);
        assert_eq!(reduced_stream(&s), None);
        assert_eq!(mirror_picture(&s, fast, false), (false, s.clone()));
        assert_eq!(mirror_picture(&s, 0, true), (false, s));
    }

    #[test]
    fn mirror_picture_follows_the_bitrate() {
        let full = stream(2560, 1440, 60);
        let small = stream(1280, 720, 30);
        // 4 % of 1440p60 (the floor) is about 0.9 Mbit/s: smaller.
        let floor = nominal_bps(Mode::Mirror, &full) * u64::from(MIN_SCALE) / 100;
        assert_eq!(mirror_picture(&full, floor, false), (true, small.clone()));
        assert_eq!(mirror_picture(&full, 4_000_000, true), (true, small));
        assert_eq!(mirror_picture(&full, 5_000_000, true), (false, full.clone()));
        assert_eq!(mirror_picture(&full, 3_000_000, false), (false, full));
    }
}
