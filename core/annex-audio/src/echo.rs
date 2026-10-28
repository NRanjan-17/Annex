//! Echo cancellation (WebRTC's AEC3, through `sonora`): what this computer plays aloud is taken
//! back out of what it sends, so the other side doesn't hear itself.
//!
//! A viewer plays the other computer's sound and sends its microphone: without this, the mic
//! picks the speakers up and the other side hears its own sound come back. A host plays the
//! viewer's microphone aloud and sends its own sound, which then contains that voice.

use sonora::config::{EchoCanceller, NoiseSuppression};
use sonora::{AudioProcessing, Config, StreamConfig};

use crate::RATE;

/// AEC3 works on 10 ms blocks.
const BLOCK: usize = RATE as usize / 100;
/// Blocks after the last played sound before sound that isn't a voice passes untouched (2 s).
const IDLE_BLOCKS: u32 = 200;

pub struct Echo {
    apm: AudioProcessing,
    /// Played sound waiting for a whole block.
    played: Vec<f32>,
    /// Sent sound waiting for a whole block, and cleaned sound waiting to go out.
    sent: Vec<f32>,
    clean: Vec<f32>,
    scratch: Vec<f32>,
    voice: bool,
    /// Blocks sent since sound last played here.
    since_played: u32,
}

impl Echo {
    /// `voice`: what's sent is a microphone, so background noise is taken out too. Otherwise
    /// (a computer's own sound: music, videos, games) only the echo is, and only while something
    /// plays here; the rest of the time it passes untouched.
    pub fn new(voice: bool) -> Echo {
        let config = Config {
            echo_canceller: Some(EchoCanceller::default()),
            noise_suppression: voice.then(NoiseSuppression::default),
            ..Default::default()
        };
        let apm = AudioProcessing::builder()
            .config(config)
            .capture_config(StreamConfig::new(RATE, 1))
            .render_config(StreamConfig::new(RATE, 1))
            .build();
        Echo {
            apm,
            played: Vec::new(),
            sent: Vec::new(),
            clean: Vec::new(),
            scratch: vec![0.0; BLOCK],
            voice,
            since_played: IDLE_BLOCKS,
        }
    }

    /// Sound this computer is playing aloud (48 kHz mono, any length).
    pub fn played(&mut self, samples: &[f32]) {
        self.played.extend_from_slice(samples);
        self.since_played = 0;
        let whole = self.played.len() / BLOCK * BLOCK;
        for block in self.played[..whole].as_chunks::<BLOCK>().0 {
            let _ = self.apm.process_render_f32(&[&block[..]], &mut [&mut self.scratch[..]]);
        }
        self.played.drain(..whole);
    }

    /// Takes the echo out of sound about to be sent (48 kHz mono), in place. A frame that isn't
    /// whole blocks comes out up to one block late.
    pub fn clean(&mut self, frame: &mut [f32]) {
        self.sent.extend_from_slice(frame);
        let whole = self.sent.len() / BLOCK * BLOCK;
        for block in self.sent[..whole].as_chunks::<BLOCK>().0 {
            self.since_played = self.since_played.saturating_add(1);
            let idle = !self.voice && self.since_played > IDLE_BLOCKS;
            if !idle && self.apm.process_capture_f32(&[&block[..]], &mut [&mut self.scratch[..]]).is_ok() {
                self.clean.extend_from_slice(&self.scratch);
            } else {
                self.clean.extend_from_slice(block);
            }
        }
        self.sent.drain(..whole);
        // Until enough has come out (only at the start, for odd frame sizes), silence.
        let n = frame.len().min(self.clean.len());
        let pad = frame.len() - n;
        frame[..pad].fill(0.0);
        frame[pad..].copy_from_slice(&self.clean[..n]);
        self.clean.drain(..n);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::FRAME;

    fn energy(s: &[f32]) -> f32 {
        s.iter().map(|x| x * x).sum::<f32>() / s.len().max(1) as f32
    }

    /// Far-end noise played aloud, heard back 30 ms later at half volume: after a few seconds
    /// the cleaned microphone carries far less of it.
    #[test]
    fn played_sound_is_taken_out_of_the_microphone() {
        let mut echo = Echo::new(false);
        let mut seed = 1u32;
        let mut noise = || {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (seed >> 8) as f32 / (1 << 24) as f32 - 0.5
        };
        let delay = RATE as usize * 30 / 1000;
        let mut history = vec![0.0f32; delay];
        let (mut before, mut after) = (0.0, 0.0);
        let frames = 6 * RATE as usize / FRAME;
        for i in 0..frames {
            let far: Vec<f32> = (0..FRAME).map(|_| noise() * 0.5).collect();
            echo.played(&far);
            history.extend_from_slice(&far);
            let mut mic: Vec<f32> = history[..FRAME].iter().map(|x| x * 0.5).collect();
            history.drain(..FRAME);
            let raw = energy(&mic);
            echo.clean(&mut mic);
            if i >= frames - RATE as usize / FRAME {
                before += raw;
                after += energy(&mic);
            }
        }
        let erle_db = 10.0 * (before / after.max(1e-12)).log10();
        assert!(erle_db > 15.0, "only {erle_db:.1} dB of echo removed");
    }

    /// A computer's own sound with nothing playing here comes out exactly as it went in.
    #[test]
    fn music_passes_untouched_while_nothing_plays_here() {
        let mut echo = Echo::new(false);
        let music: Vec<f32> = (0..FRAME * 10).map(|i| (i as f32 * 0.05).sin() * 0.3).collect();
        let mut out = Vec::new();
        for f in music.chunks(FRAME) {
            let mut f = f.to_vec();
            echo.clean(&mut f);
            out.extend(f);
        }
        assert_eq!(out, music);
    }

    #[test]
    fn a_frame_comes_out_the_same_length() {
        let mut echo = Echo::new(true);
        let mut f = vec![0.1f32; FRAME];
        echo.clean(&mut f);
        assert_eq!(f.len(), FRAME);
    }
}
