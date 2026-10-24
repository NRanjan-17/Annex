//! Monotonic microsecond clock and NTP-style offset estimation.

use std::collections::VecDeque;
use std::sync::OnceLock;
use std::time::Instant;

static EPOCH: OnceLock<Instant> = OnceLock::new();

/// Microseconds on this process's monotonic clock. All capture timestamps use this clock.
pub fn now_us() -> i64 {
    EPOCH.get_or_init(Instant::now).elapsed().as_micros() as i64
}

/// Estimates `host_clock - viewer_clock` from ping/pong samples, trusting the sample with
/// the smallest round-trip time among the most recent ones (least queuing noise).
#[derive(Debug, Default)]
pub struct ClockSync {
    samples: VecDeque<(i64, i64)>, // (rtt_us, offset_us)
}

const WINDOW: usize = 16;

impl ClockSync {
    /// `t0`/`t1`: viewer send/receive times; `t_host`: host time when it answered.
    /// `t0` and `t_host` come back from the peer, so arithmetic on them is checked.
    pub fn add_sample(&mut self, t0: i64, t_host: i64, t1: i64) {
        let Some(rtt) = t1.checked_sub(t0).filter(|r| *r >= 0) else { return };
        let Some(offset) = t0.checked_add(rtt / 2).and_then(|mid| t_host.checked_sub(mid)) else { return };
        if self.samples.len() == WINDOW {
            self.samples.pop_front();
        }
        self.samples.push_back((rtt, offset));
    }

    fn best(&self) -> Option<(i64, i64)> {
        self.samples.iter().copied().min_by_key(|(rtt, _)| *rtt)
    }

    pub fn offset_us(&self) -> Option<i64> {
        self.best().map(|(_, o)| o)
    }

    pub fn rtt_us(&self) -> Option<i64> {
        self.best().map(|(r, _)| r)
    }

    /// Converts a host timestamp to this (viewer) clock.
    pub fn to_local(&self, host_ts: i64) -> Option<i64> {
        self.offset_us().and_then(|o| host_ts.checked_sub(o))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_min_rtt_sample() {
        let mut c = ClockSync::default();
        // True offset 1000; one noisy sample with a large asymmetric delay.
        c.add_sample(0, 1000 + 5000, 6000);
        c.add_sample(10_000, 10_000 + 1000 + 100, 10_200);
        assert_eq!(c.offset_us(), Some(1000));
        assert_eq!(c.rtt_us(), Some(200));
        assert_eq!(c.to_local(5000), Some(4000));
    }

    #[test]
    fn ignores_time_travel_and_bounds_window() {
        let mut c = ClockSync::default();
        c.add_sample(10, 0, 5);
        assert_eq!(c.offset_us(), None);
        for i in 0..100 {
            c.add_sample(i * 10, i * 10 + 5, i * 10 + 10);
        }
        assert_eq!(c.samples.len(), WINDOW);
    }

    #[test]
    fn extreme_peer_timestamps_do_not_overflow() {
        let mut c = ClockSync::default();
        c.add_sample(i64::MIN, 0, i64::MAX);
        c.add_sample(0, i64::MIN, i64::MAX);
        c.add_sample(1, i64::MIN, 3);
        assert_eq!(c.offset_us(), None);
        c.add_sample(0, -1000, 0);
        assert_eq!(c.to_local(i64::MAX), None);
        assert_eq!(c.to_local(0), Some(1000));
    }

    #[test]
    fn monotonic() {
        let a = now_us();
        let b = now_us();
        assert!(b >= a);
    }
}
