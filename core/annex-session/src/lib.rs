//! Annex session layer: TLS transport, host/viewer state machines, trust prompts,
//! clock synchronisation and latency statistics.
//!
//! Everything is async (tokio). The FFI crate owns the runtime and turns [`Event`]s into
//! JSON for the native apps.

pub mod clock;
mod conn;
mod core;
mod events;
mod host;
mod internet;
mod pairing;
mod punch;
pub mod rate;
pub mod rendezvous;
pub mod stats;
#[doc(hidden)]
pub mod stun;
pub mod transfer;
mod transport;
mod viewer;

pub use crate::core::{Core, FAILURE_WINDOW, MAX_PAIRING_FAILURES, TrustPolicy};
pub use events::{Event, EventSink, FrameOut, FrameSink, LinkKind, TransportKind};
pub use host::{
    HostConfig, HostHandle, MAX_H264, MAX_SESSIONS, SessionId, SubmitResult, extend_stream_for, start_host,
};
pub use internet::{DEFAULT_STUN, InternetServers, OwnRelay, resolve_stun, share_code};
pub use punch::TurnServer;
#[doc(hidden)]
pub use punch::set_relay_refresh;
pub use stats::LatencyStats;
pub use transport::is_link_local;
pub use viewer::{InternetSocket, JoinCode, ViewerConfig, ViewerHandle, connect_viewer};

pub use annex_crypto::{KeyStorage, TrustedPeer};
pub use annex_proto::{Codec, DEFAULT_PORT, DisplayInfo, InputEvent, Mode, MouseButton, Quality, StreamConfig, keys};

#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    #[error("i/o: {0}")]
    Io(#[from] std::io::Error),
    #[error("protocol: {0}")]
    Proto(#[from] annex_proto::ProtoError),
    #[error("crypto: {0}")]
    Crypto(#[from] annex_crypto::CryptoError),
    #[error("timed out: {0}")]
    Timeout(&'static str),
    #[error("peer is not trusted")]
    NotTrusted,
    #[error("rejected by peer: {0}")]
    Rejected(String),
    #[error("protocol violation: {0}")]
    Violation(String),
    #[error("closed")]
    Closed,
}

impl From<tokio::time::error::Elapsed> for SessionError {
    fn from(_: tokio::time::error::Elapsed) -> Self {
        SessionError::Timeout("operation")
    }
}
