//! Windows media pipeline for Annex (viewer side in M1): Media Foundation H.264 decode and a
//! Direct3D 11 full-screen renderer. The platform-independent helpers in [`geometry`] are
//! unit-tested on every OS; the Windows parts compile only on Windows.

pub mod geometry;

#[cfg(windows)]
mod win;

#[cfg(windows)]
pub use win::{MediaError, VideoWindow, WindowHooks, primary_display_info};
