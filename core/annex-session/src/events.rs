use std::sync::Arc;

use annex_proto::{DisplayInfo, InputEvent, Mode, StreamConfig};
use serde::Serialize;

pub use crate::transport::TransportKind;

/// "direct": the peers talk over a link-local address (a cable between the two computers);
/// "network": through the LAN.
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LinkKind {
    Direct,
    Network,
    /// "internet": across the internet, found through the rendezvous server.
    Internet,
    /// "relayed": across the internet through a relay (TURN), when a direct path wasn't possible.
    Relayed,
}

use crate::LatencyStats;

/// Everything the native UI needs to know, delivered as JSON through the FFI.
/// Field names are part of the app ↔ core contract; see `docs/PROTOCOL.md` (FFI events).
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    Listening {
        port: u16,
    },
    /// Host, sharing over the internet: the code for the viewer to type, valid `expires_in`
    /// seconds and once.
    InternetCode {
        code: String,
        expires_in: u32,
    },
    /// Host: a viewer typed the code and is connecting.
    InternetJoined {
        peer_name: String,
    },
    /// Host: the viewer couldn't connect directly. `relayed`: it was given this computer's
    /// address on a relay (`limited`: ours, with its time and bitrate limits); otherwise there was
    /// no relay to give.
    InternetRelay {
        relayed: bool,
        limited: bool,
    },
    /// Host: the code no longer works. `reason`: "used", "expired", "stopped", or what went wrong.
    InternetCodeEnded {
        reason: String,
    },
    Connecting {
        address: String,
    },
    /// Host: show this PIN so the user can type it on the viewer. Cancel with
    /// `Core::cancel_request`.
    PairingPin {
        request_id: u64,
        pin: String,
        peer_name: String,
        fingerprint_display: String,
    },
    /// Viewer: ask the user for the PIN shown on the host; answer with `Core::submit_pin`.
    PairingPinRequired {
        request_id: u64,
        peer_name: String,
        fingerprint_display: String,
    },
    /// Pairing finished on this side (dismiss the PIN UI).
    PairingFinished {
        request_id: u64,
        success: bool,
        peer_name: String,
    },
    /// `session_id` is 0 on the viewer side (a viewer has one session).
    SessionStarted {
        session_id: u64,
        mode: Mode,
        peer_name: String,
        fingerprint: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        viewer_display: Option<DisplayInfo>,
        stream: StreamConfig,
        transport: TransportKind,
        link: LinkKind,
        /// The viewer's mouse and keyboard control the host in this session.
        input: bool,
        /// The host understands pointer movement ([`annex_proto::InputEvent::MouseDelta`],
        /// protocol 1.2), so the viewer may send that instead of positions.
        pointer_deltas: bool,
    },
    /// Input was allowed or revoked during a session (both sides).
    InputAllowed {
        session_id: u64,
        allowed: bool,
    },
    /// Host: the viewer wants to control this computer but its device isn't allowed yet. Ask the
    /// user; allowing it (`Core::set_allow_input`) takes effect within about a second.
    InputRequested {
        session_id: u64,
        peer_name: String,
        fingerprint: String,
    },
    /// The other side asks for a direct Wi-Fi link. Answer with an offer (start a hotspot) or
    /// "unavailable".
    DirectLinkRequested {
        session_id: u64,
    },
    /// The other side runs a hotspot for a direct link: join it, then reconnect (viewer) or keep
    /// sharing (host). The passphrase only travels inside the encrypted session.
    DirectLinkOffer {
        session_id: u64,
        ssid: String,
        passphrase: String,
    },
    /// The other side can't set up a direct link.
    DirectLinkUnavailable {
        session_id: u64,
        reason: String,
    },
    /// Host: inject this input into the session's display. Only delivered while allowed.
    Input {
        session_id: u64,
        event: InputEvent,
    },
    StreamConfig {
        stream: StreamConfig,
    },
    KeyframeRequested {
        session_id: u64,
    },
    /// Host: adaptive bitrate. Set the session's encoder to `scale_percent` % of its normal bitrate.
    BitrateHint {
        session_id: u64,
        scale_percent: u32,
    },
    LatencyStats {
        #[serde(flatten)]
        stats: LatencyStats,
        #[serde(skip_serializing_if = "Option::is_none")]
        clock_rtt_ms: Option<f64>,
    },
    SessionEnded {
        session_id: u64,
        reason: String,
    },
    /// The other computer copied something: put it on this one's clipboard (if the user shares
    /// the clipboard). `mime`: "text/plain;charset=utf-8" or "image/png". `data` is base64 in JSON.
    ClipboardReceived {
        session_id: u64,
        mime: String,
        #[serde(serialize_with = "base64")]
        data: Vec<u8>,
    },
    /// A file started going either way. `incoming`: from the other computer to this one.
    TransferStarted {
        session_id: u64,
        id: u64,
        incoming: bool,
        name: String,
        size: u64,
    },
    /// Bytes done so far, about four times a second.
    TransferProgress {
        session_id: u64,
        id: u64,
        incoming: bool,
        done: u64,
    },
    /// Finished. `path`: where a received file was saved. `error`: why it failed, for the user.
    TransferEnded {
        session_id: u64,
        id: u64,
        incoming: bool,
        ok: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        path: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    Error {
        message: String,
    },
}

pub type EventSink = Arc<dyn Fn(Event) + Send + Sync>;

/// Standard base64 with padding, for bytes inside JSON events.
fn base64<S: serde::Serializer>(data: &[u8], s: S) -> Result<S::Ok, S::Error> {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for c in data.chunks(3) {
        let n =
            (u32::from(c[0]) << 16) | (u32::from(*c.get(1).unwrap_or(&0)) << 8) | u32::from(*c.get(2).unwrap_or(&0));
        for (i, shift) in [18, 12, 6, 0].into_iter().enumerate() {
            out.push(if i <= c.len() { A[(n >> shift & 63) as usize] as char } else { '=' });
        }
    }
    s.serialize_str(&out)
}

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
