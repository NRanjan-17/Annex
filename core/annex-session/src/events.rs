use std::sync::Arc;

use annex_proto::{DisplayInfo, StreamConfig};
use serde::Serialize;

use crate::LatencyStats;

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PeerRole {
    Host,
    Viewer,
}

/// Everything the native UI needs to know, delivered as JSON through the FFI.
/// Field names are part of the app ↔ core contract; see `docs/PROTOCOL.md` (FFI events).
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    Listening {
        port: u16,
    },
    Connecting {
        address: String,
    },
    /// An unknown peer wants to connect. Answer with `Core::respond_trust`.
    TrustRequest {
        request_id: u64,
        peer_name: String,
        peer_role: PeerRole,
        fingerprint: String,
        fingerprint_display: String,
    },
    SessionStarted {
        peer_name: String,
        fingerprint: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        viewer_display: Option<DisplayInfo>,
        stream: StreamConfig,
    },
    StreamConfig {
        stream: StreamConfig,
    },
    KeyframeRequested,
    LatencyStats {
        #[serde(flatten)]
        stats: LatencyStats,
        #[serde(skip_serializing_if = "Option::is_none")]
        clock_rtt_ms: Option<f64>,
    },
    SessionEnded {
        reason: String,
    },
    Error {
        message: String,
    },
}

pub type EventSink = Arc<dyn Fn(Event) + Send + Sync>;

/// A received video frame, borrowed for the duration of the callback.
#[derive(Debug)]
pub struct FrameOut<'a> {
    pub frame_id: u32,
    /// Capture time converted to the local monotonic clock, once clock sync has converged.
    pub capture_ts_local_us: Option<i64>,
    pub keyframe: bool,
    pub data: &'a [u8],
}

pub type FrameSink = Arc<dyn Fn(FrameOut<'_>) + Send + Sync>;
