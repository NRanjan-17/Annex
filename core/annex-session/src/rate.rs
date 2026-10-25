//! Adaptive bitrate: turns viewer receiver reports into an encoder bitrate scale and an FEC level.
//!
//! Loss-based with a delay guard, in the spirit of GCC/TFRC but deliberately simple:
//! - a lost frame, heavy datagram loss (> 5 %), or RTT rising well above the best seen →
//!   multiplicative decrease (× 0.8) and more parity;
//! - RTT creeping up (≥ 1.5 × the best + 8 ms, the first sign of a Wi-Fi queue building) →
//!   gentle decrease (× 0.9) before anything is lost;
//! - moderate datagram loss (2–5 %) → hold the rate, add parity;
//! - clean intervals → additive increase (+5 % of the nominal rate) and slowly less parity.
//!
//! The scale is a percentage of the app's normal bitrate for the stream, so the core needs no
//! codec knowledge.

use std::collections::VecDeque;

use annex_proto::{Mode, Quality, ReceiverReport, StreamConfig};

pub const MIN_SCALE: u32 = 20;
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

/// Our relay's bitrate limit. With its login lifetime (20 minutes) that's at most about 0.9 GB a
/// session, so the daily login cap keeps it inside Cloudflare's free relay allowance.
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
        let (rtt_congested, rtt_rising) = match (best_rtt_us, r.rtt_us) {
            (_, 0) | (None, _) => (false, false),
            (Some(best), rtt) => (
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

        if r.lost > 0 || shard_loss > 0.05 || rtt_congested {
            self.scale = (self.scale * 4 / 5).max(MIN_SCALE.min(self.ceiling));
            self.fec = (self.fec + 5).min(MAX_FEC);
        } else if rtt_rising {
            self.scale = (self.scale * 9 / 10).max(MIN_SCALE.min(self.ceiling));
        } else if shard_loss > 0.02 {
            self.fec = (self.fec + 2).min(MAX_FEC);
        } else if r.frames > 0 {
            self.scale = (self.scale + 5).min(self.ceiling);
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
    fn moderate_loss_holds_rate_and_adds_parity() {
        let mut c = RateController::default();
        let d = c.on_report(&report(60, 0, 1000, 30, 3000)); // ~2.9 % recovered
        assert_eq!((d.scale_percent, d.fec_percent, d.scale_changed), (100, 17, false));
    }

    #[test]
    fn rising_delay_is_treated_as_congestion() {
        let mut c = RateController::default();
        c.on_report(&report(60, 0, 1500, 0, 4000));
        let d = c.on_report(&report(60, 0, 1500, 0, 40_000)); // bufferbloat: RTT 10x
        assert!(d.scale_changed && d.scale_percent < 105);
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
    fn creeping_delay_backs_off_gently_before_loss() {
        let mut c = RateController::default();
        c.on_report(&report(60, 0, 1500, 0, 4000));
        // 4 ms best → 20 ms is above 1.5x + 8 ms, below the 2x + 15 ms congestion line.
        let d = c.on_report(&report(60, 0, 1500, 0, 20_000));
        // The clean first report raised it to 105 %; the gentle back-off is ×0.9.
        assert_eq!((d.scale_percent, d.scale_changed), (94, true));
    }

    #[test]
    fn a_brief_low_rtt_is_forgotten() {
        let mut c = RateController::default();
        for _ in 0..3 {
            c.on_report(&report(60, 0, 1500, 0, 3000));
        }
        // The path settles at a steady 30 ms: congested at first compared to 3 ms, but once the
        // low samples age out the rate climbs again instead of staying pinned at the floor.
        for _ in 0..RTT_WINDOW + 30 {
            c.on_report(&report(60, 0, 1500, 0, 30_000));
        }
        assert_eq!(c.scale_percent(), MAX_SCALE);
    }

    #[test]
    fn huge_peer_counts_do_not_overflow() {
        let mut c = RateController::default();
        let d = c.on_report(&report(u32::MAX, 0, u32::MAX, u32::MAX, u32::MAX));
        assert!(d.scale_changed, "half the datagrams rebuilt is heavy loss");
    }

    #[test]
    fn idle_intervals_change_nothing() {
        let mut c = RateController::default();
        let d = c.on_report(&report(0, 0, 0, 0, 0));
        assert_eq!((d.scale_percent, d.scale_changed), (100, false));
    }
}
