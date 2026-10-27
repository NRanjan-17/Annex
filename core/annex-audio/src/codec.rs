use opus::{Application, Bitrate, Channels};

/// Compresses 20 ms frames with Opus.
pub struct Encoder {
    inner: opus::Encoder,
    buf: Vec<u8>,
}

impl Encoder {
    /// `voice` tunes it for speech (the microphone); otherwise for any sound.
    pub fn new(voice: bool, bits_per_second: i32) -> Result<Encoder, String> {
        let app = if voice { Application::Voip } else { Application::Audio };
        let mut inner = opus::Encoder::new(crate::RATE, Channels::Mono, app).map_err(|e| e.to_string())?;
        inner.set_bitrate(Bitrate::Bits(bits_per_second)).map_err(|e| e.to_string())?;
        Ok(Encoder { inner, buf: vec![0; 1500] })
    }

    /// One frame (`FRAME` samples) in, one packet out.
    pub fn encode(&mut self, frame: &[f32]) -> Result<&[u8], String> {
        let n = self.inner.encode_float(frame, &mut self.buf).map_err(|e| e.to_string())?;
        Ok(&self.buf[..n])
    }
}

/// Expands Opus packets back to 20 ms frames, filling in for lost ones.
pub struct Decoder {
    inner: opus::Decoder,
    next_seq: Option<u32>,
    pcm: Vec<f32>,
}

impl Decoder {
    pub fn new() -> Result<Decoder, String> {
        Ok(Decoder {
            inner: opus::Decoder::new(crate::RATE, Channels::Mono).map_err(|e| e.to_string())?,
            next_seq: None,
            pcm: vec![0.0; crate::FRAME * 6],
        })
    }

    /// Decodes packet `seq`, first covering up to three lost packets before it. Late or repeated
    /// packets are dropped. Calls `frame` for each frame made.
    pub fn decode(&mut self, seq: u32, packet: &[u8], mut frame: impl FnMut(&[f32])) -> Result<(), String> {
        if let Some(next) = self.next_seq {
            let ahead = seq.wrapping_sub(next);
            if ahead > u32::MAX / 2 {
                return Ok(());
            }
            for _ in 0..ahead.min(3) {
                // A lost packet: Opus makes up one frame's worth (the length asked for).
                let n =
                    self.inner.decode_float(&[], &mut self.pcm[..crate::FRAME], false).map_err(|e| e.to_string())?;
                frame(&self.pcm[..n]);
            }
        }
        self.next_seq = Some(seq.wrapping_add(1));
        let n = self.inner.decode_float(packet, &mut self.pcm, false).map_err(|e| e.to_string())?;
        frame(&self.pcm[..n]);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tone_survives_the_round_trip_and_losses_are_filled() {
        let mut enc = Encoder::new(false, 48_000).unwrap();
        let mut dec = Decoder::new().unwrap();
        let tone: Vec<f32> = (0..crate::FRAME).map(|i| (i as f32 * 0.05).sin() * 0.5).collect();
        let mut frames = 0;
        for seq in [0u32, 1, 2, 5, 6, 4] {
            let packet = enc.encode(&tone).unwrap().to_vec();
            assert!(packet.len() < 400, "{}", packet.len());
            dec.decode(seq, &packet, |f| {
                assert_eq!(f.len(), crate::FRAME);
                frames += 1;
            })
            .unwrap();
        }
        // 0, 1, 2, two filled in for 3 and 4, then 5 and 6; the late 4 is dropped.
        assert_eq!(frames, 7);
    }
}
