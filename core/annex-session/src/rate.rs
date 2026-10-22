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

use annex_proto::{Quality, ReceiverReport};

pub const MIN_SCALE: u32 = 20;
pub const MAX_SCALE: u32 = 150;
pub const MIN_FEC: u32 = 10;
pub const MAX_FEC: u32 = 40;

#[derive(Debug, Clone)]
pub struct RateController {
    scale: u32,
    fec: u32,
    best_rtt_us: Option<u32>,
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
        RateController { scale: ceiling.min(100), fec: 15, best_rtt_us: None, ceiling }
    }

    pub fn scale_percent(&self) -> u32 {
        self.scale
    }

    pub fn fec_percent(&self) -> u32 {
        self.fec
    }

    pub fn on_report(&mut self, r: &ReceiverReport) -> RateDecision {
        let before = self.scale;
        let shard_loss =
            if r.shards + r.recovered == 0 { 0.0 } else { r.recovered as f64 / (r.shards + r.recovered) as f64 };
        let (rtt_congested, rtt_rising) = match (self.best_rtt_us, r.rtt_us) {
            (_, 0) | (None, _) => (false, false),
            (Some(best), rtt) => (
                rtt > best.saturating_mul(2).saturating_add(15_000),
                rtt > best.saturating_add(best / 2).saturating_add(8_000),
            ),
        };
        if r.rtt_us > 0 {
            self.best_rtt_us = Some(self.best_rtt_us.map_or(r.rtt_us, |b| b.min(r.rtt_us)));
        }

        if r.lost > 0 || shard_loss > 0.05 || rtt_congested {
            self.scale = (self.scale * 4 / 5).max(MIN_SCALE);
            self.fec = (self.fec + 5).min(MAX_FEC);
        } else if rtt_rising {
            self.scale = (self.scale * 9 / 10).max(MIN_SCALE);
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
    fn idle_intervals_change_nothing() {
        let mut c = RateController::default();
        let d = c.on_report(&report(0, 0, 0, 0, 0));
        assert_eq!((d.scale_percent, d.scale_changed), (100, false));
    }
}
