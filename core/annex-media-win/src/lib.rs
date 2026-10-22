//! Windows media pipeline for Annex. Viewer: Media Foundation H.264 decode (hardware via
//! D3D11) and a Direct3D 11 full-screen renderer. Host: Windows.Graphics.Capture → D3D11 video
//! processor → Media Foundation H.264 encode (hardware MFT or software). The platform-independent helpers in [`geometry`] are
//! unit-tested on every OS; the Windows parts compile only on Windows.

pub mod annexb;
pub mod geometry;
pub mod idd;

#[cfg(windows)]
mod win;

#[cfg(windows)]
pub use win::*;
