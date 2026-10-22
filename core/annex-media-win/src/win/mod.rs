mod capture;
mod convert;
mod decoder;
mod display;
mod encoder;
mod renderer;
mod screen_host;
mod window;

pub use decoder::{DecodedFrame, H264Decoder, Picture};
pub use display::{Monitor, monitors, primary_display_info};
pub use encoder::{EncoderConfig, OutputFn, SoftwareEncoder};
pub use screen_host::{EncodedFrame, HostPipeline, HostPipelineConfig, stream_size};
pub use window::{VideoWindow, WindowHooks};

#[derive(Debug, thiserror::Error)]
pub enum MediaError {
    #[error("windows: {0}")]
    Win(#[from] windows::core::Error),
    #[error("{0}")]
    Other(String),
}
