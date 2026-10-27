use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::thread::JoinHandle;
use std::time::Duration;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SampleFormat, SizedSample};

use crate::Converter;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// The default microphone.
    Microphone,
    /// Everything this computer is playing. On a Mac the app supplies it (ScreenCaptureKit), so
    /// starting it here fails; see [`Capture::system_sound_here`].
    SystemSound,
}

/// A running capture; it stops when dropped. Frames are 48 kHz mono, `FRAME` samples each, and
/// arrive on the capture's own thread.
pub struct Capture {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

type Ready = mpsc::Sender<Result<(), String>>;

impl Capture {
    /// Whether this library can capture the computer's own sound here (Windows and Linux).
    pub fn system_sound_here() -> bool {
        cfg!(any(windows, target_os = "linux"))
    }

    pub fn start(source: Source, on_frame: impl FnMut(&[f32]) + Send + 'static) -> Result<Capture, String> {
        if source == Source::SystemSound && !Capture::system_sound_here() {
            return Err("this computer's sound comes from the app here".into());
        }
        let stop = Arc::new(AtomicBool::new(false));
        let (ready_tx, ready_rx) = mpsc::channel();
        let s = stop.clone();
        let thread = std::thread::Builder::new()
            .name(format!("audio {source:?}"))
            .spawn(move || run(source, on_frame, &s, ready_tx))
            .map_err(|e| e.to_string())?;
        match ready_rx.recv_timeout(Duration::from_secs(10)) {
            Ok(Ok(())) => Ok(Capture { stop, thread: Some(thread) }),
            Ok(Err(e)) => {
                let _ = thread.join();
                Err(e)
            }
            Err(_) => {
                stop.store(true, Ordering::Relaxed);
                Err("the sound device didn't start".into())
            }
        }
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn run(source: Source, on_frame: impl FnMut(&[f32]) + Send + 'static, stop: &AtomicBool, ready: Ready) {
    #[cfg(target_os = "linux")]
    if source == Source::SystemSound {
        return pulse::record(on_frame, stop, ready);
    }
    let host = cpal::default_host();
    let (device, config) = match source {
        Source::Microphone => (host.default_input_device(), "microphone"),
        // WASAPI records what an output device plays when asked for input from it (loopback).
        Source::SystemSound => (host.default_output_device(), "speakers"),
    };
    let Some(device) = device else {
        let _ = ready.send(Err(format!("no {config} found")));
        return;
    };
    let config = match source {
        Source::Microphone => device.default_input_config(),
        Source::SystemSound => device.default_output_config(),
    };
    let config = match config {
        Ok(c) => c,
        Err(e) => {
            let _ = ready.send(Err(e.to_string()));
            return;
        }
    };
    tracing::info!(
        target: "annex::audio",
        ?source,
        rate = config.sample_rate(),
        channels = config.channels(),
        format = ?config.sample_format(),
        "capturing"
    );
    let stream = match config.sample_format() {
        SampleFormat::F32 => build::<f32>(&device, &config, on_frame),
        SampleFormat::I16 => build::<i16>(&device, &config, on_frame),
        SampleFormat::I32 => build::<i32>(&device, &config, on_frame),
        SampleFormat::U16 => build::<u16>(&device, &config, on_frame),
        SampleFormat::U8 => build::<u8>(&device, &config, on_frame),
        f => Err(format!("unsupported sample format {f:?}")),
    };
    let stream = match stream.and_then(|s| s.play().map(|()| s).map_err(|e| e.to_string())) {
        Ok(s) => s,
        Err(e) => {
            let _ = ready.send(Err(e));
            return;
        }
    };
    let _ = ready.send(Ok(()));
    while !stop.load(Ordering::Relaxed) {
        std::thread::sleep(Duration::from_millis(50));
    }
    drop(stream);
    tracing::info!(target: "annex::audio", ?source, "capture stopped");
}

fn build<T>(
    device: &cpal::Device,
    config: &cpal::SupportedStreamConfig,
    mut on_frame: impl FnMut(&[f32]) + Send + 'static,
) -> Result<cpal::Stream, String>
where
    T: SizedSample,
    f32: FromSample<T>,
{
    let mut conv = Converter::new(config.sample_rate(), config.channels());
    let mut buf = Vec::<f32>::new();
    device
        .build_input_stream::<T, _, _>(
            config.config(),
            move |data: &[T], _: &cpal::InputCallbackInfo| {
                buf.clear();
                buf.extend(data.iter().map(|&s| s.to_sample::<f32>()));
                conv.push(&buf, &mut on_frame);
            },
            |e| tracing::warn!(target: "annex::audio", "capture error: {e}"),
            None,
        )
        .map_err(|e| e.to_string())
}

/// The computer's own sound on Linux: PulseAudio and PipeWire both offer it as the default
/// output's "monitor", which `parec` records.
#[cfg(target_os = "linux")]
mod pulse {
    use std::io::Read;
    use std::process::{Command, Stdio};
    use std::sync::atomic::{AtomicBool, Ordering};

    use super::Ready;
    use crate::Converter;

    pub(super) fn record(mut on_frame: impl FnMut(&[f32]), stop: &AtomicBool, ready: Ready) {
        let child = Command::new("parec")
            .args(["--device=@DEFAULT_MONITOR@", "--format=float32le", "--rate=48000", "--channels=1"])
            .args(["--raw", "--latency-msec=20"])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn();
        let mut child = match child {
            Ok(c) => c,
            Err(e) => {
                let _ = ready.send(Err(format!("parec (pulseaudio-utils) didn't start: {e}")));
                return;
            }
        };
        let Some(mut out) = child.stdout.take() else {
            let _ = ready.send(Err("parec has no output".into()));
            return;
        };
        tracing::info!(target: "annex::audio", "capturing this computer's sound (parec, default monitor)");
        let _ = ready.send(Ok(()));
        let mut conv = Converter::new(crate::RATE, 1);
        let (mut bytes, mut pending, mut samples) = (vec![0u8; 8192], Vec::<u8>::new(), Vec::<f32>::new());
        while !stop.load(Ordering::Relaxed) {
            let n = match out.read(&mut bytes) {
                Ok(0) | Err(_) => break,
                Ok(n) => n,
            };
            pending.extend_from_slice(&bytes[..n]);
            let whole = pending.len() / 4 * 4;
            samples.clear();
            samples.extend(pending[..whole].chunks_exact(4).map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]])));
            pending.drain(..whole);
            conv.push(&samples, &mut on_frame);
        }
        let _ = child.kill();
        let _ = child.wait();
        tracing::info!(target: "annex::audio", "stopped capturing this computer's sound");
    }
}
