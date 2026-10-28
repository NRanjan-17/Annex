//! Sound between two computers: the microphone, the computer's own sound (what it's playing),
//! Opus compression and playback with a small jitter buffer.
//!
//! Everything travels as 48 kHz mono in 20 ms frames. Capture converts whatever the device gives
//! to that; playback converts it to whatever the device wants. Nothing here ever logs or keeps
//! the sound itself, only formats, counts and errors.

mod capture;
mod codec;
mod convert;
mod echo;
mod play;

pub use capture::{Capture, Source};
pub use codec::{Decoder, Encoder};
pub use convert::Converter;
pub use echo::Echo;
pub use play::{Output, Player};

/// Samples per second, everywhere between the computers.
pub const RATE: u32 = 48_000;
/// Samples in one frame (20 ms).
pub const FRAME: usize = 960;
