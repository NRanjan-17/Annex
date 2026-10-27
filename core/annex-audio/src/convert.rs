/// Turns interleaved samples at any rate and channel count into 48 kHz mono frames.
pub struct Converter {
    channels: usize,
    /// Input samples per output sample.
    step: f64,
    /// Position of the next output sample, in input samples after `prev`.
    pos: f64,
    prev: f32,
    out: Vec<f32>,
}

impl Converter {
    pub fn new(rate: u32, channels: u16) -> Converter {
        Converter {
            channels: usize::from(channels.max(1)),
            step: f64::from(rate.max(1)) / f64::from(crate::RATE),
            pos: 0.0,
            prev: 0.0,
            out: Vec::with_capacity(crate::FRAME * 2),
        }
    }

    /// Adds interleaved samples; calls `frame` for each full 20 ms frame.
    pub fn push(&mut self, interleaved: &[f32], mut frame: impl FnMut(&[f32])) {
        for chunk in interleaved.chunks_exact(self.channels) {
            let cur = chunk.iter().sum::<f32>() / self.channels as f32;
            // Output samples falling between `prev` (at 0) and `cur` (at 1).
            while self.pos < 1.0 {
                let t = self.pos as f32;
                self.out.push(self.prev + (cur - self.prev) * t);
                self.pos += self.step;
                if self.out.len() == crate::FRAME {
                    frame(&self.out);
                    self.out.clear();
                }
            }
            self.pos -= 1.0;
            self.prev = cur;
        }
    }
}

/// Resamples 48 kHz mono to `rate` (still mono), for playback.
pub(crate) struct Upsampler {
    step: f64,
    pos: f64,
    prev: f32,
}

impl Upsampler {
    pub(crate) fn new(rate: u32) -> Upsampler {
        Upsampler { step: f64::from(crate::RATE) / f64::from(rate.max(1)), pos: 0.0, prev: 0.0 }
    }

    pub(crate) fn push(&mut self, mono: &[f32], out: &mut impl Extend<f32>) {
        for &cur in mono {
            while self.pos < 1.0 {
                let t = self.pos as f32;
                out.extend([self.prev + (cur - self.prev) * t]);
                self.pos += self.step;
            }
            self.pos -= 1.0;
            self.prev = cur;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frames(rate: u32, channels: u16, seconds: f64) -> usize {
        let mut c = Converter::new(rate, channels);
        let input = vec![0.5f32; (f64::from(rate) * seconds) as usize * usize::from(channels)];
        let mut n = 0;
        c.push(&input, |f| {
            assert_eq!(f.len(), crate::FRAME);
            n += 1;
        });
        n
    }

    #[test]
    fn any_rate_becomes_20_ms_frames() {
        assert_eq!(frames(48_000, 1, 1.0), 50);
        assert_eq!(frames(48_000, 2, 1.0), 50);
        assert!((49..=50).contains(&frames(44_100, 2, 1.0)));
        assert!((49..=50).contains(&frames(16_000, 1, 1.0)));
        assert!((49..=50).contains(&frames(96_000, 8, 1.0)));
    }

    #[test]
    fn channels_are_averaged() {
        let mut c = Converter::new(48_000, 2);
        let input: Vec<f32> = std::iter::repeat_n([1.0, 0.0], 2000).flatten().collect();
        c.push(&input, |f| assert!(f[10..].iter().all(|&s| (s - 0.5).abs() < 1e-6)));
    }

    #[test]
    fn upsampling_keeps_the_length_in_time() {
        let mut u = Upsampler::new(44_100);
        let mut out = Vec::new();
        u.push(&[0.0; 48_000], &mut out);
        assert!((44_090..=44_110).contains(&out.len()), "{}", out.len());
    }
}
