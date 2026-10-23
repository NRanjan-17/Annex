mod capture;
mod convert;
mod decoder;
mod display;
mod encoder;
mod input;
mod renderer;
mod screen_host;
mod virtual_display;
mod wifi;
mod window;

pub use decoder::{DecodedFrame, H264Decoder, Picture};
pub use display::{Monitor, monitors, primary_display_info};
pub use encoder::{EncoderConfig, OutputFn, SoftwareEncoder};
pub use input::Injector;
pub use screen_host::{EncodedFrame, Gpu, HostPipeline, HostPipelineConfig, gpus, stream_size};
pub use virtual_display::{DriverState, VirtualDisplayDriver, driver_state, monitor_size};
pub use wifi::wifi_low_latency;
pub use window::{VideoWindow, WindowHooks};

#[derive(Debug, thiserror::Error)]
pub enum MediaError {
    #[error("windows: {0}")]
    Win(#[from] windows::core::Error),
    #[error("{0}")]
    Other(String),
}

/// Starts Media Foundation once per process. It is never shut down: MFShutdown while any MF
/// object is still alive (e.g. a transform released after its owner's Drop body ran) crashes.
pub(crate) fn mf_startup() -> Result<(), MediaError> {
    use std::sync::OnceLock;
    static STARTED: OnceLock<Result<(), String>> = OnceLock::new();
    STARTED
        .get_or_init(|| unsafe {
            windows::Win32::Media::MediaFoundation::MFStartup(
                windows::Win32::Media::MediaFoundation::MF_VERSION,
                windows::Win32::Media::MediaFoundation::MFSTARTUP_NOSOCKET,
            )
            .map_err(|e| e.to_string())
        })
        .clone()
        .map_err(MediaError::Other)
}
