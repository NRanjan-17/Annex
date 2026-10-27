use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SampleFormat, SizedSample};

use crate::convert::Upsampler;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Output {
    /// The default speakers or headphones.
    Speakers,
    /// "Annex Microphone": an input other apps (calls, recorders) can choose.
    VirtualMicrophone,
}

/// Plays 48 kHz mono frames as they arrive, keeping about 60 ms buffered against network jitter.
/// It stops when dropped.
pub struct Player {
    inner: Inner,
}

enum Inner {
    Device {
        queue: Arc<Mutex<Jitter>>,
        up: Upsampler,
        stop: Arc<AtomicBool>,
        thread: Option<JoinHandle<()>>,
    },
    #[cfg(target_os = "linux")]
    Pulse(pulse::VirtualMic),
}

impl Player {
    /// Whether "Annex Microphone" can be made here. Linux only for now: PulseAudio and PipeWire
    /// can add an input without a driver.
    pub fn virtual_microphone_here() -> bool {
        cfg!(target_os = "linux")
    }

    pub fn start(output: Output) -> Result<Player, String> {
        match output {
            Output::Speakers => start_device(),
            #[cfg(target_os = "linux")]
            Output::VirtualMicrophone => Ok(Player { inner: Inner::Pulse(pulse::VirtualMic::start()?) }),
            #[cfg(not(target_os = "linux"))]
            Output::VirtualMicrophone => Err("Annex Microphone isn't available on this system yet".into()),
        }
    }

    /// Queues one frame (48 kHz mono).
    pub fn push(&mut self, frame: &[f32]) {
        match &mut self.inner {
            Inner::Device { queue, up, .. } => {
                let mut q = queue.lock().unwrap();
                up.push(frame, &mut q.samples);
                q.trim();
            }
            #[cfg(target_os = "linux")]
            Inner::Pulse(mic) => mic.push(frame),
        }
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        #[allow(irrefutable_let_patterns)] // only one kind off Linux
        if let Inner::Device { stop, thread, .. } = &mut self.inner {
            stop.store(true, Ordering::Relaxed);
            if let Some(t) = thread.take() {
                let _ = t.join();
            }
        }
    }
}

/// Samples waiting to be played, at the device's rate.
struct Jitter {
    samples: VecDeque<f32>,
    playing: bool,
    /// Buffered before starting (and after running dry).
    target: usize,
    /// More than this and the oldest are dropped back to `target`, so delay can't build up.
    most: usize,
}

impl Jitter {
    fn new(rate: u32) -> Jitter {
        let ms = |n: u32| (rate as usize) * n as usize / 1000;
        Jitter { samples: VecDeque::new(), playing: false, target: ms(60), most: ms(250) }
    }

    fn next(&mut self) -> f32 {
        if !self.playing {
            if self.samples.len() < self.target {
                return 0.0;
            }
            self.playing = true;
        }
        self.samples.pop_front().unwrap_or_else(|| {
            self.playing = false;
            0.0
        })
    }

    fn trim(&mut self) {
        if self.samples.len() > self.most {
            let extra = self.samples.len() - self.target;
            self.samples.drain(..extra);
        }
    }
}

fn start_device() -> Result<Player, String> {
    let stop = Arc::new(AtomicBool::new(false));
    let (ready_tx, ready_rx) = mpsc::channel::<Result<(u32, Arc<Mutex<Jitter>>), String>>();
    let s = stop.clone();
    let thread = std::thread::Builder::new()
        .name("audio playback".into())
        .spawn(move || {
            let opened = cpal::default_host()
                .default_output_device()
                .ok_or_else(|| "no speakers found".to_string())
                .and_then(|d| d.default_output_config().map(|c| (d, c)).map_err(|e| e.to_string()));
            let (device, config) = match opened {
                Ok(x) => x,
                Err(e) => {
                    let _ = ready_tx.send(Err(e));
                    return;
                }
            };
            let queue = Arc::new(Mutex::new(Jitter::new(config.sample_rate())));
            let q = queue.clone();
            let stream = match config.sample_format() {
                SampleFormat::F32 => build::<f32>(&device, &config, q),
                SampleFormat::I16 => build::<i16>(&device, &config, q),
                SampleFormat::I32 => build::<i32>(&device, &config, q),
                SampleFormat::U16 => build::<u16>(&device, &config, q),
                SampleFormat::U8 => build::<u8>(&device, &config, q),
                f => Err(format!("unsupported sample format {f:?}")),
            };
            let stream = match stream.and_then(|s| s.play().map(|()| s).map_err(|e| e.to_string())) {
                Ok(s) => s,
                Err(e) => {
                    let _ = ready_tx.send(Err(e));
                    return;
                }
            };
            tracing::info!(
                target: "annex::audio",
                rate = config.sample_rate(),
                channels = config.channels(),
                format = ?config.sample_format(),
                "playing"
            );
            let _ = ready_tx.send(Ok((config.sample_rate(), queue)));
            while !s.load(Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(50));
            }
            drop(stream);
            tracing::info!(target: "annex::audio", "playback stopped");
        })
        .map_err(|e| e.to_string())?;
    match ready_rx.recv_timeout(Duration::from_secs(10)) {
        Ok(Ok((rate, queue))) => {
            Ok(Player { inner: Inner::Device { queue, up: Upsampler::new(rate), stop, thread: Some(thread) } })
        }
        Ok(Err(e)) => {
            let _ = thread.join();
            Err(e)
        }
        Err(_) => {
            stop.store(true, Ordering::Relaxed);
            Err("the speakers didn't start".into())
        }
    }
}

fn build<T>(
    device: &cpal::Device,
    config: &cpal::SupportedStreamConfig,
    queue: Arc<Mutex<Jitter>>,
) -> Result<cpal::Stream, String>
where
    T: SizedSample + FromSample<f32>,
{
    let channels = usize::from(config.channels().max(1));
    device
        .build_output_stream::<T, _, _>(
            config.config(),
            move |data: &mut [T], _: &cpal::OutputCallbackInfo| {
                let mut q = queue.lock().unwrap();
                for frame in data.chunks_mut(channels) {
                    let s = T::from_sample(q.next());
                    frame.fill(s);
                }
            },
            |e| tracing::warn!(target: "annex::audio", "playback error: {e}"),
            None,
        )
        .map_err(|e| e.to_string())
}

/// "Annex Microphone" on Linux: a null sink Annex plays into, and its monitor remapped as an
/// input named "Annex Microphone". Both are removed when playback stops.
#[cfg(target_os = "linux")]
mod pulse {
    use std::io::Write;
    use std::process::{Child, ChildStdin, Command, Stdio};

    const SINK: &str = "annex_mic";

    pub(super) struct VirtualMic {
        modules: Vec<String>,
        child: Child,
        input: ChildStdin,
        bytes: Vec<u8>,
    }

    fn pactl(args: &[&str]) -> Result<String, String> {
        let out = Command::new("pactl").args(args).output().map_err(|e| format!("pactl: {e}"))?;
        if !out.status.success() {
            return Err(format!("pactl {}: {}", args.join(" "), String::from_utf8_lossy(&out.stderr).trim()));
        }
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    }

    /// Unloads Annex's modules left behind by a crash.
    fn clean_up() {
        let Ok(list) = pactl(&["list", "short", "modules"]) else { return };
        for line in list.lines().filter(|l| l.contains(SINK)) {
            if let Some(index) = line.split_whitespace().next() {
                let _ = pactl(&["unload-module", index]);
            }
        }
    }

    impl VirtualMic {
        pub(super) fn start() -> Result<VirtualMic, String> {
            clean_up();
            let mut modules = Vec::new();
            let made = (|| {
                modules.push(pactl(&[
                    "load-module",
                    "module-null-sink",
                    &format!("sink_name={SINK}"),
                    "rate=48000",
                    "channels=1",
                    "sink_properties=device.description=Annex-Microphone-Feed",
                ])?);
                modules.push(pactl(&[
                    "load-module",
                    "module-remap-source",
                    &format!("master={SINK}.monitor"),
                    "source_name=annex_microphone",
                    "source_properties=device.description=\"Annex Microphone\"",
                ])?);
                Command::new("pacat")
                    .args(["--playback", &format!("--device={SINK}"), "--format=float32le", "--rate=48000"])
                    .args(["--channels=1", "--raw", "--latency-msec=60"])
                    .stdin(Stdio::piped())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .spawn()
                    .map_err(|e| format!("pacat (pulseaudio-utils) didn't start: {e}"))
            })();
            match made {
                Ok(mut child) => {
                    let input = child.stdin.take().ok_or("pacat has no input")?;
                    tracing::info!(target: "annex::audio", "Annex Microphone added");
                    Ok(VirtualMic { modules, child, input, bytes: Vec::new() })
                }
                Err(e) => {
                    for m in modules.iter().rev() {
                        let _ = pactl(&["unload-module", m]);
                    }
                    Err(e)
                }
            }
        }

        pub(super) fn push(&mut self, frame: &[f32]) {
            self.bytes.clear();
            self.bytes.extend(frame.iter().flat_map(|s| s.to_le_bytes()));
            let _ = self.input.write_all(&self.bytes);
        }
    }

    impl Drop for VirtualMic {
        fn drop(&mut self) {
            let _ = self.child.kill();
            let _ = self.child.wait();
            for m in self.modules.iter().rev() {
                let _ = pactl(&["unload-module", m]);
            }
            tracing::info!(target: "annex::audio", "Annex Microphone removed");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jitter_buffer_waits_then_plays_and_caps_the_delay() {
        let mut j = Jitter::new(48_000);
        j.samples.extend([1.0; 1000]);
        assert_eq!(j.next(), 0.0, "waits for 60 ms");
        j.samples.extend([1.0; 2000]);
        assert_eq!(j.next(), 1.0);
        j.samples.extend([1.0; 20_000]);
        j.trim();
        assert_eq!(j.samples.len(), j.target, "no more than 250 ms behind");
        j.samples.clear();
        assert_eq!(j.next(), 0.0);
        assert!(!j.playing, "runs dry, then waits again");
    }
}
