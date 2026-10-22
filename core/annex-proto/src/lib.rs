//! Annex wire protocol: record framing, control messages and video frame headers.
//!
//! This crate is pure (no I/O) so it can be unit-tested and fuzzed everywhere.
//! The format is specified in `docs/PROTOCOL.md`; keep the two in sync.

use serde::{Deserialize, Serialize};

pub mod fragment;
pub mod keys;
mod record;
mod video;

pub use record::{
    DecodedRecord, MAX_CONTROL_LEN, MAX_RECORD_LEN, RECORD_HEADER_LEN, RecordHeader, RecordType, decode_record,
    encode_record,
};
pub use video::{VIDEO_HEADER_LEN, VideoFlags, VideoFrame, VideoHeader};

/// Major protocol version. Changes only for incompatible changes and is part of the ALPN.
pub const PROTOCOL_MAJOR: u16 = 1;
/// Minor protocol version. Bumped for backwards-compatible additions.
pub const PROTOCOL_MINOR: u16 = 1;
/// TLS ALPN identifier. Peers that don't speak the same major version fail the handshake.
pub const ALPN: &[u8] = b"annex/1";
/// Default TCP port for the M1 transport.
pub const DEFAULT_PORT: u16 = 47800;
/// mDNS service type for the TCP transport.
pub const MDNS_SERVICE_TCP: &str = "_annex._tcp.local.";
/// Longest accepted device name, in bytes.
pub const MAX_NAME_LEN: usize = 64;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ProtoError {
    #[error("record too large: {0} bytes")]
    RecordTooLarge(usize),
    #[error("unknown record type {0:#04x}")]
    UnknownRecordType(u8),
    #[error("truncated data")]
    Truncated,
    #[error("malformed control message: {0}")]
    Malformed(String),
    #[error("invalid field: {0}")]
    InvalidField(&'static str),
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Host,
    Viewer,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    Mirror,
    Extend,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum Codec {
    /// H.264 Annex B byte stream.
    H264,
    /// HEVC Annex B byte stream.
    Hevc,
    /// Opaque test payloads (benchmarks and CI only, never decoded).
    Raw,
}

/// Physical properties of the viewer's display, used to pick stream size and, in
/// Extend mode, the virtual display's default mode and physical size.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct DisplayInfo {
    pub width_px: u32,
    pub height_px: u32,
    #[serde(default)]
    pub width_mm: u32,
    #[serde(default)]
    pub height_mm: u32,
    #[serde(default)]
    pub refresh_hz: u32,
    /// Backing scale factor in percent (100 = 1x, 200 = Retina 2x).
    #[serde(default = "default_scale")]
    pub scale_percent: u32,
}

fn default_scale() -> u32 {
    100
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct Hello {
    pub proto_major: u16,
    pub proto_minor: u16,
    pub device_name: String,
    /// Hex SHA-256 fingerprint of the sender's certificate. Must match the TLS peer certificate.
    pub device_id: String,
    #[serde(default)]
    pub app_version: String,
    #[serde(default)]
    pub roles: Vec<Role>,
    #[serde(default)]
    pub caps: Vec<String>,
    /// True if the sender already trusts the receiver's certificate. If either side sends
    /// false, the peers run PIN pairing before the session starts.
    #[serde(default)]
    pub knows_peer: bool,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct SessionRequest {
    pub mode: Mode,
    pub display: DisplayInfo,
    pub codecs: Vec<Codec>,
    /// The viewer wants to control the host with its mouse and keyboard.
    #[serde(default)]
    pub input: bool,
    /// How hard the host may push the link (bitrate ceiling).
    #[serde(default)]
    pub quality: Quality,
}

/// Viewer preference that caps the host's bitrate: `Fast` suits Wi-Fi, `Best` a cable.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum Quality {
    #[default]
    Best,
    Balanced,
    Fast,
}

/// Longest Wi-Fi network name (IEEE 802.11).
pub const MAX_SSID_LEN: usize = 32;

/// Largest value of a normalized pointer coordinate (the right or bottom edge of the stream).
pub const POINTER_MAX: u16 = u16::MAX;

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MouseButton {
    Left,
    Right,
    Middle,
}

/// One viewer input event (M6). Pointer positions are normalized to the stream: 0 is the left
/// or top edge, [`POINTER_MAX`] the right or bottom edge, so they don't depend on either side's
/// resolution or scaling.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(tag = "k", rename_all = "snake_case")]
pub enum InputEvent {
    MouseMove {
        x: u16,
        y: u16,
    },
    MouseButton {
        button: MouseButton,
        down: bool,
    },
    /// Scroll distance in pixels. Positive `dy` moves the content down (scrolling up), positive
    /// `dx` moves it right, matching both platforms' wheel conventions.
    Scroll {
        dx: i32,
        dy: i32,
    },
    /// A key, as a USB HID usage on the keyboard page (0x07); see [`keys`].
    Key {
        usage: u16,
        down: bool,
    },
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct StreamConfig {
    pub codec: Codec,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
}

/// Reception statistics for one reporting interval.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, Default)]
pub struct ReceiverReport {
    /// Frames delivered to the decoder.
    pub frames: u32,
    /// Frames given up on (couldn't be rebuilt).
    pub lost: u32,
    /// Video datagrams received (QUIC only).
    #[serde(default)]
    pub shards: u32,
    /// Datagrams rebuilt from FEC parity (QUIC only): a measure of packet loss.
    #[serde(default)]
    pub recovered: u32,
    /// Smoothed round-trip time in microseconds.
    #[serde(default)]
    pub rtt_us: u32,
}

/// Control messages. Encoded as a CBOR map whose `t` key holds the snake_case type name.
/// Unknown keys are ignored and unknown types are skipped, so minor versions can add both.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum Control {
    Hello(Hello),
    SessionRequest(SessionRequest),
    SessionAccept {
        stream: StreamConfig,
        /// The host accepts input from this viewer (it asked, and the host allows it).
        #[serde(default)]
        input: bool,
    },
    SessionReject {
        reason: String,
    },
    StreamConfig(StreamConfig),
    KeyframeRequest,
    /// SPAKE2 message (viewer = side A, host = side B).
    PairSpake {
        #[serde(with = "serde_bytes")]
        msg: Vec<u8>,
    },
    /// HMAC key confirmation over the pairing transcript (32 bytes).
    PairConfirm {
        #[serde(with = "serde_bytes")]
        mac: Vec<u8>,
    },
    /// Viewer → host, about once a second: how the last interval went (drives adaptive bitrate).
    ReceiverReport(ReceiverReport),
    /// Viewer → host: mouse or keyboard input. Only sent while input is allowed; hosts drop it otherwise.
    Input(InputEvent),
    /// Host → viewer: input was allowed or revoked during the session.
    InputAllowed {
        allowed: bool,
    },
    /// Viewer → host: please set up a direct Wi-Fi link (one side runs a hotspot).
    DirectLinkRequest,
    /// Either way: join this Wi-Fi network for a direct link, then reconnect. Sent only inside the
    /// encrypted, mutually authenticated session; the passphrase is random per offer.
    DirectLinkOffer {
        ssid: String,
        passphrase: String,
    },
    /// Host → viewer: a direct link isn't possible here (e.g. no hotspot support).
    DirectLinkUnavailable {
        reason: String,
    },
    ClockPing {
        seq: u32,
        t_viewer_us: i64,
    },
    ClockPong {
        seq: u32,
        t_viewer_us: i64,
        t_host_us: i64,
    },
    Bye {
        reason: String,
    },
}

const KNOWN_CONTROL_TYPES: &[&str] = &[
    "hello",
    "session_request",
    "session_accept",
    "session_reject",
    "stream_config",
    "keyframe_request",
    "pair_spake",
    "receiver_report",
    "input",
    "input_allowed",
    "direct_link_request",
    "direct_link_offer",
    "direct_link_unavailable",
    "pair_confirm",
    "clock_ping",
    "clock_pong",
    "bye",
];

impl Control {
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(128);
        ciborium::into_writer(self, &mut out).expect("serializing to a Vec cannot fail");
        out
    }

    /// Decodes a control message. Returns `Ok(None)` for well-formed messages of a type this
    /// version doesn't know, which callers must ignore for forward compatibility.
    pub fn decode(bytes: &[u8]) -> Result<Option<Control>, ProtoError> {
        if bytes.len() > MAX_CONTROL_LEN {
            return Err(ProtoError::RecordTooLarge(bytes.len()));
        }
        let value: ciborium::Value = ciborium::from_reader(bytes).map_err(|e| ProtoError::Malformed(e.to_string()))?;
        let type_name = value
            .as_map()
            .and_then(|m| m.iter().find(|(k, _)| k.as_text() == Some("t")).and_then(|(_, v)| v.as_text()))
            .ok_or_else(|| ProtoError::Malformed("missing type tag".into()))?;
        if !KNOWN_CONTROL_TYPES.contains(&type_name) {
            return Ok(None);
        }
        let msg: Control = value.deserialized().map_err(|e| ProtoError::Malformed(e.to_string()))?;
        msg.validate()?;
        Ok(Some(msg))
    }

    /// Semantic checks beyond what the type system enforces.
    pub fn validate(&self) -> Result<(), ProtoError> {
        match self {
            Control::Hello(h) => {
                if h.device_name.len() > MAX_NAME_LEN {
                    return Err(ProtoError::InvalidField("device_name"));
                }
                if h.device_id.len() != 64 || !h.device_id.bytes().all(|b| b.is_ascii_hexdigit()) {
                    return Err(ProtoError::InvalidField("device_id"));
                }
            }
            Control::SessionRequest(r) => {
                if r.display.width_px == 0 || r.display.height_px == 0 {
                    return Err(ProtoError::InvalidField("display"));
                }
                if r.codecs.is_empty() {
                    return Err(ProtoError::InvalidField("codecs"));
                }
            }
            Control::SessionAccept { stream, .. } | Control::StreamConfig(stream) => {
                if stream.width == 0 || stream.height == 0 || stream.fps == 0 {
                    return Err(ProtoError::InvalidField("stream"));
                }
            }
            Control::DirectLinkOffer { ssid, passphrase } => {
                // WPA2-PSK: 1–32 byte SSID, 8–63 character passphrase.
                if ssid.is_empty() || ssid.len() > MAX_SSID_LEN {
                    return Err(ProtoError::InvalidField("ssid"));
                }
                if !(8..=63).contains(&passphrase.len()) {
                    return Err(ProtoError::InvalidField("passphrase"));
                }
            }
            Control::SessionReject { reason } | Control::Bye { reason } | Control::DirectLinkUnavailable { reason } => {
                if reason.len() > 256 {
                    return Err(ProtoError::InvalidField("reason"));
                }
            }
            Control::PairSpake { msg } => {
                if msg.is_empty() || msg.len() > 128 {
                    return Err(ProtoError::InvalidField("msg"));
                }
            }
            Control::PairConfirm { mac } => {
                if mac.len() != 32 {
                    return Err(ProtoError::InvalidField("mac"));
                }
            }
            Control::Input(InputEvent::Scroll { dx, dy }) => {
                // One event is at most a few screens' worth; anything bigger is garbage.
                if dx.unsigned_abs() > 100_000 || dy.unsigned_abs() > 100_000 {
                    return Err(ProtoError::InvalidField("scroll"));
                }
            }
            Control::Input(_)
            | Control::DirectLinkRequest
            | Control::InputAllowed { .. }
            | Control::KeyframeRequest
            | Control::ClockPing { .. }
            | Control::ClockPong { .. }
            | Control::ReceiverReport(_) => {}
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hello() -> Hello {
        Hello {
            proto_major: PROTOCOL_MAJOR,
            proto_minor: PROTOCOL_MINOR,
            device_name: "Test Mac".into(),
            device_id: "ab".repeat(32),
            app_version: "0.1.0".into(),
            roles: vec![Role::Host, Role::Viewer],
            caps: vec!["h264".into()],
            knows_peer: true,
        }
    }

    #[test]
    fn control_roundtrip() {
        let msgs = vec![
            Control::Hello(hello()),
            Control::SessionRequest(SessionRequest {
                mode: Mode::Mirror,
                display: DisplayInfo {
                    width_px: 2560,
                    height_px: 1600,
                    width_mm: 300,
                    height_mm: 190,
                    refresh_hz: 60,
                    scale_percent: 200,
                },
                codecs: vec![Codec::H264],
                input: false,
                quality: Quality::Fast,
            }),
            Control::SessionAccept {
                stream: StreamConfig { codec: Codec::H264, width: 1920, height: 1080, fps: 60 },
                input: true,
            },
            Control::Input(InputEvent::MouseMove { x: 0, y: POINTER_MAX }),
            Control::Input(InputEvent::MouseButton { button: MouseButton::Right, down: true }),
            Control::Input(InputEvent::Scroll { dx: -3, dy: 120 }),
            Control::Input(InputEvent::Key { usage: 0x04, down: false }),
            Control::InputAllowed { allowed: false },
            Control::DirectLinkRequest,
            Control::DirectLinkOffer { ssid: "Annex-ab12".into(), passphrase: "x".repeat(16) },
            Control::DirectLinkUnavailable { reason: "no_hotspot".into() },
            Control::SessionReject { reason: "busy".into() },
            Control::KeyframeRequest,
            Control::PairSpake { msg: vec![1; 33] },
            Control::PairConfirm { mac: vec![2; 32] },
            Control::ClockPing { seq: 7, t_viewer_us: -5 },
            Control::ReceiverReport(ReceiverReport { frames: 60, lost: 1, shards: 900, recovered: 12, rtt_us: 4000 }),
            Control::ClockPong { seq: 7, t_viewer_us: -5, t_host_us: 123_456_789 },
            Control::Bye { reason: "user".into() },
        ];
        for m in msgs {
            let bytes = m.encode();
            assert_eq!(Control::decode(&bytes).unwrap(), Some(m));
        }
    }

    #[test]
    fn unknown_type_is_skipped() {
        let mut bytes = Vec::new();
        let v = ciborium::Value::Map(vec![(
            ciborium::Value::Text("t".into()),
            ciborium::Value::Text("from_the_future".into()),
        )]);
        ciborium::into_writer(&v, &mut bytes).unwrap();
        assert_eq!(Control::decode(&bytes).unwrap(), None);
    }

    #[test]
    fn unknown_fields_are_ignored_and_defaults_apply() {
        let v = ciborium::Value::Map(vec![
            (ciborium::Value::Text("t".into()), ciborium::Value::Text("session_request".into())),
            (ciborium::Value::Text("mode".into()), ciborium::Value::Text("mirror".into())),
            (
                ciborium::Value::Text("display".into()),
                ciborium::Value::Map(vec![
                    (ciborium::Value::Text("width_px".into()), 1920.into()),
                    (ciborium::Value::Text("height_px".into()), 1080.into()),
                ]),
            ),
            (
                ciborium::Value::Text("codecs".into()),
                ciborium::Value::Array(vec![ciborium::Value::Text("h264".into())]),
            ),
            (ciborium::Value::Text("new_field".into()), 42.into()),
        ]);
        let mut bytes = Vec::new();
        ciborium::into_writer(&v, &mut bytes).unwrap();
        let Some(Control::SessionRequest(r)) = Control::decode(&bytes).unwrap() else {
            panic!("expected session_request");
        };
        assert_eq!(r.display.scale_percent, 100);
        assert!(!r.input);
    }

    #[test]
    fn rejects_bad_device_id_and_oversized_name() {
        let mut h = hello();
        h.device_id = "zz".repeat(32);
        assert!(Control::decode(&Control::Hello(h).encode()).is_err());
        let mut h = hello();
        h.device_name = "x".repeat(MAX_NAME_LEN + 1);
        assert!(Control::decode(&Control::Hello(h).encode()).is_err());
    }

    #[test]
    fn pairing_fields_are_validated() {
        assert!(Control::decode(&Control::PairConfirm { mac: vec![0; 31] }.encode()).is_err());
        assert!(Control::decode(&Control::PairSpake { msg: vec![] }.encode()).is_err());
        assert!(Control::decode(&Control::PairSpake { msg: vec![0; 129] }.encode()).is_err());
    }

    #[test]
    fn input_wire_format_is_flat() {
        // `t` names the control message and `k` the input kind, side by side in one map.
        let bytes = Control::Input(InputEvent::Key { usage: 0x29, down: true }).encode();
        let v: ciborium::Value = ciborium::from_reader(&bytes[..]).unwrap();
        let keys: Vec<&str> = v.as_map().unwrap().iter().filter_map(|(k, _)| k.as_text()).collect();
        assert_eq!(keys, ["t", "k", "usage", "down"]);
        assert!(Control::decode(&Control::Input(InputEvent::Scroll { dx: 0, dy: 200_000 }).encode()).is_err());
    }

    #[test]
    fn direct_link_offer_is_validated() {
        let bad = |ssid: &str, passphrase: &str| {
            Control::decode(&Control::DirectLinkOffer { ssid: ssid.into(), passphrase: passphrase.into() }.encode())
                .is_err()
        };
        assert!(bad("", "12345678"));
        assert!(bad(&"s".repeat(33), "12345678"));
        assert!(bad("Annex", "short"));
        assert!(bad("Annex", &"p".repeat(64)));
        assert!(!bad("Annex", "12345678"));
    }

    #[test]
    fn missing_tag_is_malformed() {
        let mut bytes = Vec::new();
        ciborium::into_writer(&ciborium::Value::Integer(5.into()), &mut bytes).unwrap();
        assert!(matches!(Control::decode(&bytes), Err(ProtoError::Malformed(_))));
    }
}
