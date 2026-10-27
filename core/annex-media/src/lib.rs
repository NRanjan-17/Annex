//! Media helpers every platform's pipeline shares: H.264 Annex B handling, letterboxing, pointer
//! coordinates, NV12 copies and stream sizes. Pure Rust, unit-tested on every OS.

pub mod annexb;
pub mod geometry;
