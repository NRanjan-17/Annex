//! Windows only: Media Foundation software H.264 encode → Annex B → decode, on the CI runner.
#![cfg(windows)]

use std::sync::{Arc, Mutex};

use annex_media_win::annexb::nal_types;
use annex_media_win::{EncodedFrame, EncoderConfig, H264Decoder, Picture, SoftwareEncoder};

fn frame(w: u32, h: u32, shade: u8) -> Vec<u8> {
    let mut v = vec![shade; (w * h) as usize];
    // A moving diagonal so frames differ.
    for y in 0..h {
        let x = (y + shade as u32 * 3) % w;
        v[(y * w + x) as usize] = 255 - shade;
    }
    v.extend(std::iter::repeat_n(128u8, (w * h / 2) as usize));
    v
}

#[test]
fn software_encode_then_decode() {
    let (w, h) = (640u32, 360u32);
    let out: Arc<Mutex<Vec<EncodedFrame>>> = Arc::default();
    let sink = out.clone();
    let enc = match SoftwareEncoder::new(
        EncoderConfig { width: w, height: h, fps: 60, bitrate: 2_000_000 },
        Arc::new(move |f| sink.lock().unwrap().push(f)),
    ) {
        Ok(e) => e,
        Err(e) => {
            // Windows Server images without the Media Foundation codecs.
            eprintln!("SKIP: Microsoft H.264 encoder unavailable: {e}");
            return;
        }
    };
    for i in 0..30u8 {
        if i == 15 {
            enc.request_keyframe();
        }
        enc.encode(&frame(w, h, 16 + i * 4), i as i64 * 16_667).unwrap();
    }
    drop(enc);
    let frames = out.lock().unwrap().clone();
    assert!(frames.len() >= 20, "encoder produced only {} frames", frames.len());
    assert!(frames[0].keyframe, "first frame must be a keyframe");
    assert!(nal_types(&frames[0].data).contains(&7), "keyframe must carry SPS: {:?}", nal_types(&frames[0].data));
    assert!(frames.iter().skip(1).any(|f| f.keyframe), "requested keyframe must be honoured");
    assert!(frames.iter().all(|f| nal_types(&f.data).iter().all(|t| *t != 0)), "every unit is Annex B");

    let mut dec = H264Decoder::new(None).expect("decoder");
    let mut pictures = 0;
    for f in &frames {
        for p in dec.decode(&f.data).expect("decode") {
            assert_eq!((p.width, p.visible_width, p.visible_height), (p.width, w, h));
            assert!(matches!(p.picture, Picture::Cpu { .. }));
            pictures += 1;
        }
    }
    assert!(pictures + 2 >= frames.len(), "decoded {pictures} of {} frames", frames.len());
}

#[test]
fn monitors_are_enumerated() {
    // Hosted runners have at least a virtual display; just make sure the call works.
    let list = annex_media_win::monitors();
    for m in &list {
        assert!(m.width > 0 && m.height > 0, "{m:?}");
    }
    eprintln!("monitors: {list:?}");
}
