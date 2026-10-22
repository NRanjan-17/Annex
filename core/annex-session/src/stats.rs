//! Rolling latency statistics (capture → present) for the viewer and the benchmark.

use serde::Serialize;

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct LatencyStats {
    pub frames: usize,
    pub fps: f64,
    pub p50_ms: f64,
    pub p95_ms: f64,
    pub p99_ms: f64,
    pub max_ms: f64,
}

#[derive(Debug, Default)]
pub struct LatencyWindow {
    samples_us: Vec<i64>,
    first_at: Option<i64>,
    last_at: i64,
}

impl LatencyWindow {
    pub fn push(&mut self, latency_us: i64, at_us: i64) {
        self.first_at.get_or_insert(at_us);
        self.last_at = at_us;
        self.samples_us.push(latency_us.max(0));
    }

    pub fn is_empty(&self) -> bool {
        self.samples_us.is_empty()
    }

    /// Computes stats and resets the window.
    pub fn take(&mut self) -> Option<LatencyStats> {
        let stats = summarize(&self.samples_us, self.first_at?, self.last_at);
        *self = LatencyWindow::default();
        stats
    }
}

/// Percentile summary of latency samples (µs) observed between `first_at` and `last_at`.
pub fn summarize(samples_us: &[i64], first_at: i64, last_at: i64) -> Option<LatencyStats> {
    if samples_us.is_empty() {
        return None;
    }
    let mut s = samples_us.to_vec();
    s.sort_unstable();
    let pct = |p: f64| {
        let idx = ((p / 100.0) * (s.len() - 1) as f64).round() as usize;
        s[idx] as f64 / 1000.0
    };
    let span_s = (last_at - first_at) as f64 / 1e6;
    let fps = if span_s > 0.0 { (s.len() - 1) as f64 / span_s } else { 0.0 };
    Some(LatencyStats {
        frames: s.len(),
        fps,
        p50_ms: pct(50.0),
        p95_ms: pct(95.0),
        p99_ms: pct(99.0),
        max_ms: *s.last().unwrap() as f64 / 1000.0,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentiles() {
        let samples: Vec<i64> = (1..=100).map(|i| i * 1000).collect();
        let s = summarize(&samples, 0, 99 * 16_667).unwrap();
        assert_eq!(s.frames, 100);
        assert_eq!(s.p50_ms, 51.0);
        assert_eq!(s.p95_ms, 95.0);
        assert_eq!(s.max_ms, 100.0);
        assert!((s.fps - 60.0).abs() < 0.1);
    }

    #[test]
    fn window_resets() {
        let mut w = LatencyWindow::default();
        assert!(w.take().is_none());
        w.push(5000, 0);
        w.push(-3, 10);
        let s = w.take().unwrap();
        assert_eq!((s.frames, s.p50_ms), (2, 5.0));
        assert!(w.is_empty());
    }
}
