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
    pub fn add_sample(&mut self, t0: i64, t_host: i64, t1: i64) {
        if t1 < t0 {
            return;
        }
        let rtt = t1 - t0;
        let offset = t_host - (t0 + rtt / 2);
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
        self.offset_us().map(|o| host_ts - o)
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
    fn monotonic() {
        let a = now_us();
        let b = now_us();
        assert!(b >= a);
    }
}
