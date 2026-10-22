//! Property tests: decoders must never panic on arbitrary input, and valid messages roundtrip.

use annex_proto::*;
use proptest::prelude::*;

fn arb_stream() -> impl Strategy<Value = StreamConfig> {
    (prop_oneof![Just(Codec::H264), Just(Codec::Hevc), Just(Codec::Raw)], 1u32..8192, 1u32..8192, 1u32..240)
        .prop_map(|(codec, width, height, fps)| StreamConfig { codec, width, height, fps })
}

fn arb_control() -> impl Strategy<Value = Control> {
    prop_oneof![
        ("[ -~]{0,64}", "[0-9a-f]{64}", any::<u16>(), any::<u16>()).prop_map(|(n, id, a, b)| Control::Hello(Hello {
            proto_major: a,
            proto_minor: b,
            device_name: n,
            device_id: id,
            app_version: String::new(),
            roles: vec![Role::Viewer],
            caps: vec![],
            knows_peer: a % 2 == 0,
        })),
        proptest::collection::vec(any::<u8>(), 1..=128).prop_map(|msg| Control::PairSpake { msg }),
        proptest::collection::vec(any::<u8>(), 32..=32).prop_map(|mac| Control::PairConfirm { mac }),
        arb_stream().prop_map(|stream| Control::SessionAccept { stream }),
        arb_stream().prop_map(Control::StreamConfig),
        (any::<u32>(), any::<i64>()).prop_map(|(seq, t_viewer_us)| Control::ClockPing { seq, t_viewer_us }),
        (any::<u32>(), any::<i64>(), any::<i64>()).prop_map(|(seq, t_viewer_us, t_host_us)| Control::ClockPong {
            seq,
            t_viewer_us,
            t_host_us
        }),
        "[ -~]{0,256}".prop_map(|reason| Control::Bye { reason }),
        Just(Control::KeyframeRequest),
    ]
}

proptest! {
    #[test]
    fn control_decode_never_panics(bytes in proptest::collection::vec(any::<u8>(), 0..512)) {
        let _ = Control::decode(&bytes);
    }

    #[test]
    fn record_decode_never_panics(bytes in proptest::collection::vec(any::<u8>(), 0..64)) {
        let _ = decode_record(&bytes);
    }

    #[test]
    fn video_decode_never_panics(bytes in proptest::collection::vec(any::<u8>(), 0..64)) {
        let _ = VideoFrame::decode(&bytes);
    }

    #[test]
    fn control_roundtrips(msg in arb_control()) {
        prop_assert_eq!(Control::decode(&msg.encode()).unwrap(), Some(msg));
    }

    #[test]
    fn video_roundtrips(frame_id: u32, ts: i64, flags: u8, data in proptest::collection::vec(any::<u8>(), 0..256)) {
        let f = VideoFrame { header: VideoHeader { frame_id, capture_ts_us: ts, flags: VideoFlags(flags) }, data };
        prop_assert_eq!(VideoFrame::decode(&f.encode()).unwrap(), f);
    }
}
