//! Screen → H.264 with GStreamer. Annex ships no codec: it uses the encoders the system has,
//! the graphics chip's first (docs/LINUX.md, "Video").

use std::sync::{Arc, Mutex};

use gst::prelude::*;

/// One encoder Annex knows how to drive.
#[derive(Clone, Copy, Debug)]
struct Encoder {
    factory: &'static str,
    hardware: bool,
    /// What goes between the scaled picture and the encoder (raw format, NVMM upload, …).
    input: Input,
    bitrate: Bitrate,
}

#[derive(Clone, Copy, Debug)]
enum Input {
    Nv12,
    I420,
    /// Jetson: nvvidconv scales and uploads into NVMM memory.
    Nvmm,
}

/// How the element's bitrate property is counted.
#[derive(Clone, Copy, Debug)]
enum Bitrate {
    Kbps,
    Bps,
    /// v4l2h264enc (Raspberry Pi 4): through `extra-controls`, fixed for the session.
    V4l2,
}

const ENCODERS: &[Encoder] = &[
    // Intel and AMD (VA-API): the new va plugin, then the older vaapi one.
    Encoder { factory: "vah264enc", hardware: true, input: Input::Nv12, bitrate: Bitrate::Kbps },
    Encoder { factory: "vah264lpenc", hardware: true, input: Input::Nv12, bitrate: Bitrate::Kbps },
    Encoder { factory: "vaapih264enc", hardware: true, input: Input::Nv12, bitrate: Bitrate::Kbps },
    // NVIDIA desktop and DGX Spark (NVENC).
    Encoder { factory: "nvh264enc", hardware: true, input: Input::Nv12, bitrate: Bitrate::Kbps },
    Encoder { factory: "nvcudah264enc", hardware: true, input: Input::Nv12, bitrate: Bitrate::Kbps },
    // Jetson (AGX Thor and Orin).
    Encoder { factory: "nvv4l2h264enc", hardware: true, input: Input::Nvmm, bitrate: Bitrate::Bps },
    // Raspberry Pi 4 (the Pi 5 has no H.264 encoder).
    Encoder { factory: "v4l2h264enc", hardware: true, input: Input::I420, bitrate: Bitrate::V4l2 },
    // Software.
    Encoder { factory: "x264enc", hardware: false, input: Input::I420, bitrate: Bitrate::Kbps },
    Encoder { factory: "openh264enc", hardware: false, input: Input::I420, bitrate: Bitrate::Bps },
];

fn usable(prefer_hardware: bool) -> impl Iterator<Item = &'static Encoder> {
    let forced = std::env::var("ANNEX_ENCODER").ok();
    ENCODERS
        .iter()
        .filter(move |e| match &forced {
            Some(f) => e.factory == f,
            None => prefer_hardware || !e.hardware,
        })
        .filter(|e| gst::ElementFactory::find(e.factory).is_some())
}

fn pick(prefer_hardware: bool) -> Option<&'static Encoder> {
    usable(prefer_hardware).next()
}

/// "x264enc (software)", for About.
pub fn describe(prefer_hardware: bool) -> String {
    match pick(prefer_hardware) {
        Some(e) => format!("{} ({})", e.factory, if e.hardware { "graphics chip" } else { "software" }),
        None => "none found (install gstreamer1.0-plugins-ugly or gstreamer1.0-plugins-bad)".into(),
    }
}

/// Whether the encoder that would be used runs on the graphics chip (bigger streams then).
pub fn hardware(prefer_hardware: bool) -> bool {
    pick(prefer_hardware).is_some_and(|e| e.hardware)
}

/// Frames get more alike as the rate goes up, so above 60 fps the bitrate grows half as fast.
/// (The same as the Windows host.)
pub fn bitrate(w: u32, h: u32, fps: u32) -> u32 {
    let rate = fps.min(60) + fps.saturating_sub(60) / 2;
    (w * h * rate / 8).clamp(4_000_000, 40_000_000)
}

pub struct EncodedFrame<'a> {
    pub data: &'a [u8],
    pub capture_us: i64,
    pub keyframe: bool,
}

pub type Output = Arc<dyn Fn(EncodedFrame<'_>) + Send + Sync>;

pub struct Config {
    /// A GStreamer source description ending in raw video (pipewiresrc, ximagesrc, appsrc, …).
    pub source: String,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub bitrate: u32,
    pub prefer_hardware: bool,
    /// Encode at most this many frames a second (adaptive bitrate's smaller picture), for
    /// sources that send as fast as the screen changes. A skipped last change still shows:
    /// pipewiresrc repeats its last frame every half second (keepalive-time).
    pub max_fps: Option<u32>,
}

/// A running capture → encode pipeline. Dropping it stops it.
pub struct Pipeline {
    pipeline: gst::Pipeline,
    encoder: gst::Element,
    sink: gst::Element,
    kind: Encoder,
    base_bitrate: u32,
}

impl Pipeline {
    pub fn start(cfg: Config, output: Output) -> Result<Pipeline, String> {
        Self::start_with(cfg, output, |_| {})
    }

    /// `setup` sees the pipeline before it plays (to find an appsrc by name).
    pub fn start_with(cfg: Config, output: Output, setup: impl FnOnce(&gst::Pipeline)) -> Result<Pipeline, String> {
        let kind = *pick(cfg.prefer_hardware).ok_or("No H.264 encoder found. Install gstreamer1.0-plugins-ugly.")?;
        if let Ok(forced) = std::env::var("ANNEX_ENCODER") {
            tracing::info!(forced, "encoder named by ANNEX_ENCODER");
        }
        let (w, h, fps) = (cfg.width & !1, cfg.height & !1, cfg.fps.max(1));
        let convert = match kind.input {
            Input::Nvmm => format!("nvvidconv ! video/x-raw(memory:NVMM),format=NV12,width={w},height={h}"),
            Input::Nv12 => format!(
                "videoconvert n-threads=4 ! videoscale n-threads=4 ! video/x-raw,format=NV12,width={w},height={h}"
            ),
            Input::I420 => format!(
                "videoconvert n-threads=4 ! videoscale n-threads=4 ! video/x-raw,format=I420,width={w},height={h}"
            ),
        };
        let desc = format!(
            "{source} ! queue max-size-buffers=1 leaky=downstream \
             ! {convert} \
             ! {enc} name=enc ! h264parse config-interval=-1 \
             ! video/x-h264,stream-format=byte-stream,alignment=au \
             ! appsink name=out sync=false max-buffers=2 drop=true",
            source = cfg.source,
            enc = kind.factory,
        );
        tracing::info!("host pipeline: {desc}");
        let pipeline = gst::parse::launch(&desc)
            .map_err(|e| format!("couldn't build the video pipeline: {e}"))?
            .downcast::<gst::Pipeline>()
            .map_err(|_| "not a pipeline")?;
        let encoder = pipeline.by_name("enc").ok_or("no encoder")?;
        configure(&encoder, &kind, cfg.bitrate, fps);
        if let Some(max) = cfg.max_fps
            && let Some(pad) = encoder.static_pad("sink")
        {
            // A little under the frame time, so a screen at exactly twice the rate isn't cut to
            // a third by timing jitter.
            let gap = gst::ClockTime::from_nseconds(900_000_000 / u64::from(max.max(1)));
            let last = Mutex::new(None::<gst::ClockTime>);
            pad.add_probe(gst::PadProbeType::BUFFER, move |_, info| {
                let Some(pts) = info.buffer().and_then(|b| b.pts()) else { return gst::PadProbeReturn::Ok };
                let mut last = last.lock().unwrap();
                if let Some(l) = *last
                    && pts >= l
                    && pts < l.saturating_add(gap)
                {
                    return gst::PadProbeReturn::Drop;
                }
                *last = Some(pts);
                gst::PadProbeReturn::Ok
            });
        }
        let sink = pipeline.by_name("out").ok_or("no appsink")?;
        let appsink = sink.clone().downcast::<gst_app::AppSink>().map_err(|_| "not an appsink")?;

        let clock_pipeline = pipeline.downgrade();
        appsink.set_callbacks(
            gst_app::AppSinkCallbacks::builder()
                .new_sample(move |s| {
                    let sample = s.pull_sample().map_err(|_| gst::FlowError::Eos)?;
                    let Some(buffer) = sample.buffer() else { return Ok(gst::FlowSuccess::Ok) };
                    let Ok(map) = buffer.map_readable() else { return Ok(gst::FlowSuccess::Ok) };
                    // How long ago the frame was captured, on the core's clock.
                    let age_us = clock_pipeline
                        .upgrade()
                        .and_then(|p| Some((p.current_running_time()?, buffer.pts()?)))
                        .map(|(now, pts)| now.saturating_sub(pts).useconds() as i64)
                        .unwrap_or(0);
                    output(EncodedFrame {
                        data: map.as_slice(),
                        capture_us: annex_session::clock::now_us() - age_us,
                        keyframe: !buffer.flags().contains(gst::BufferFlags::DELTA_UNIT),
                    });
                    Ok(gst::FlowSuccess::Ok)
                })
                .build(),
        );

        if let Some(bus) = pipeline.bus() {
            bus.set_sync_handler(|_, msg| {
                match msg.view() {
                    gst::MessageView::Error(e) => tracing::error!("host pipeline: {} ({:?})", e.error(), e.debug()),
                    gst::MessageView::Warning(w) => tracing::warn!("host pipeline: {}", w.error()),
                    _ => {}
                }
                gst::BusSyncReply::Drop
            });
        }
        setup(&pipeline);
        if let Err(e) = pipeline.set_state(gst::State::Playing) {
            let _ = pipeline.set_state(gst::State::Null);
            return Err(format!("the video pipeline didn't start: {e}"));
        }
        tracing::info!(
            encoder = kind.factory,
            hardware = kind.hardware,
            w,
            h,
            fps,
            max_fps = ?cfg.max_fps,
            bitrate = cfg.bitrate,
            "sharing: encoding"
        );
        Ok(Pipeline { pipeline, encoder, sink, kind, base_bitrate: cfg.bitrate })
    }

    pub fn request_keyframe(&self) {
        tracing::trace!(encoder = self.kind.factory, "keyframe asked for");
        let ev = gst_video::UpstreamForceKeyUnitEvent::builder().all_headers(true).build();
        self.sink.send_event(ev);
    }

    /// Adaptive bitrate: run the encoder at `percent` % of the configured bitrate.
    pub fn set_bitrate_scale(&self, percent: u32) {
        let b = (self.base_bitrate as u64 * percent as u64 / 100).clamp(500_000, u32::MAX as u64) as u32;
        tracing::debug!(encoder = self.kind.factory, percent, bitrate = b, "bitrate changed");
        set_bitrate(&self.encoder, &self.kind, b);
    }
}

impl Drop for Pipeline {
    fn drop(&mut self) {
        tracing::info!(encoder = self.kind.factory, "sharing: encoding stopped");
        let _ = self.pipeline.set_state(gst::State::Null);
    }
}

/// Sets a property when the element has it (versions differ), from its string form.
fn set(e: &gst::Element, name: &str, value: &str) {
    if e.find_property(name).is_some() {
        tracing::trace!(name, value, "encoder property");
        e.set_property_from_str(name, value);
    } else {
        tracing::debug!(name, "this encoder version has no such property; skipped");
    }
}

fn set_bitrate(e: &gst::Element, kind: &Encoder, bps: u32) {
    match kind.bitrate {
        Bitrate::Kbps => set(e, "bitrate", &(bps / 1000).to_string()),
        Bitrate::Bps => set(e, "bitrate", &bps.to_string()),
        Bitrate::V4l2 => {}
    }
}

fn configure(e: &gst::Element, kind: &Encoder, bps: u32, fps: u32) {
    // A keyframe every ~10 s; the viewer asks for one when it loses a frame.
    let gop = (fps * 10).to_string();
    set_bitrate(e, kind, bps);
    match kind.factory {
        "x264enc" => {
            set(e, "tune", "zerolatency");
            set(e, "speed-preset", "superfast");
            set(e, "key-int-max", &gop);
            set(e, "bframes", "0");
            set(e, "sliced-threads", "true");
        }
        "openh264enc" => {
            set(e, "rate-control", "bitrate");
            set(e, "complexity", "low");
            set(e, "gop-size", &gop);
            set(e, "multi-thread", "4");
        }
        "vah264enc" | "vah264lpenc" => {
            set(e, "rate-control", "cbr");
            set(e, "key-int-max", &gop);
            set(e, "b-frames", "0");
            set(e, "target-usage", "6");
        }
        "vaapih264enc" => {
            set(e, "rate-control", "cbr");
            set(e, "keyframe-period", &gop);
            set(e, "max-bframes", "0");
        }
        "nvh264enc" | "nvcudah264enc" => {
            set(e, "rc-mode", "cbr");
            set(e, "zerolatency", "true");
            set(e, "gop-size", &gop);
            set(e, "bframes", "0");
            set(e, "preset", "low-latency-hq");
        }
        "nvv4l2h264enc" => {
            set(e, "control-rate", "1");
            set(e, "iframeinterval", &gop);
            set(e, "idrinterval", &gop);
            set(e, "insert-sps-pps", "true");
            set(e, "maxperf-enable", "true");
            set(e, "preset-level", "1");
        }
        "v4l2h264enc" => {
            set(
                e,
                "extra-controls",
                &format!("controls,video_bitrate={bps},h264_i_frame_period={gop},repeat_sequence_header=1"),
            );
        }
        _ => {}
    }
}
