mod decoder;
mod display;
mod renderer;
mod window;

pub use display::primary_display_info;
pub use window::{VideoWindow, WindowHooks};

#[derive(Debug, thiserror::Error)]
pub enum MediaError {
    #[error("windows: {0}")]
    Win(#[from] windows::core::Error),
    #[error("{0}")]
    Other(String),
}
