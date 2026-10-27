//! Sound in a session, over QUIC datagrams beside the video: the shared computer's own sound to
//! the viewer, and the viewer's microphone to the shared computer, where it plays on the speakers
//! or as "Annex Microphone" for calls. Mirror sessions only, and only with peers that list the
//! [`CAP`] capability.
//!
//! The switches ([`AudioSettings`]) apply at once, to running sessions too. The sound itself is
//! never logged.

use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use annex_audio::{Capture, Converter, Decoder, Encoder, Output, Player, Source};
use annex_proto::audio::{AudioHeader, AudioKind};
use tokio::sync::mpsc;

use crate::Core;

/// Hello capability: this side sends and plays session sound.
pub const CAP: &str = "audio";

const SOUND_BITS: i32 = 64_000;
const MIC_BITS: i32 = 32_000;

/// The sound switches. Defaults: share and hear sound, microphone off, and the viewer's microphone
/// becomes "Annex Microphone" where that's possible.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioSettings {
    /// Send what this computer plays to whoever views it.
    pub share_sound: bool,
    /// Play what the other computer sends: its sound, or a viewer's microphone.
    pub hear_sound: bool,
    /// Send this computer's microphone to the computer it views.
    pub send_mic: bool,
    /// A viewer's microphone becomes "Annex Microphone" here (for calls) instead of playing aloud.
    pub virtual_mic: bool,
}

impl Default for AudioSettings {
    fn default() -> Self {
        AudioSettings { share_sound: true, hear_sound: true, send_mic: false, virtual_mic: true }
    }
}

/// Whether "Annex Microphone" can be made on this computer.
pub fn virtual_microphone_here() -> bool {
    Player::virtual_microphone_here()
}

fn now_us(start: Instant) -> i64 {
    start.elapsed().as_micros().min(i64::MAX as u128) as i64
}

/// One capture of this computer's own sound, shared by every session viewing it.
pub(crate) struct SoundHub {
    viewers: Arc<Mutex<Vec<(u64, quinn::Connection)>>>,
    capture: Mutex<Option<Capture>>,
    /// Sound handed in by the app (Mac), with the format it came in.
    pushed: Mutex<Option<Pushed>>,
    /// While a viewer's microphone plays aloud here, this computer's sound is turned down so the
    /// viewer doesn't hear themselves back. Milliseconds since `start`.
    ducked_until: Arc<AtomicU64>,
    start: Instant,
}

struct Pushed {
    rate: u32,
    channels: u16,
    conv: Converter,
    sender: Sender,
}

/// Encodes frames of this computer's sound and sends them to every viewer.
struct Sender {
    enc: Encoder,
    seq: u32,
    viewers: Arc<Mutex<Vec<(u64, quinn::Connection)>>>,
    ducked_until: Arc<AtomicU64>,
    start: Instant,
    quiet: Vec<f32>,
}

impl Sender {
    fn send(&mut self, frame: &[f32]) {
        let ms = self.start.elapsed().as_millis() as u64;
        let frame = if ms < self.ducked_until.load(Ordering::Relaxed) {
            self.quiet.clear();
            self.quiet.extend(frame.iter().map(|s| s * 0.1));
            &self.quiet[..]
        } else {
            frame
        };
        let Ok(packet) = self.enc.encode(frame) else { return };
        let h = AudioHeader { seq: self.seq, capture_ts_us: now_us(self.start), kind: AudioKind::Sound };
        let datagram = bytes::Bytes::from(h.with(packet));
        self.seq = self.seq.wrapping_add(1);
        for (_, conn) in self.viewers.lock().unwrap().iter() {
            let _ = conn.send_datagram(datagram.clone());
        }
    }
}

impl SoundHub {
    pub(crate) fn new() -> SoundHub {
        SoundHub {
            viewers: Arc::default(),
            capture: Mutex::new(None),
            pushed: Mutex::new(None),
            ducked_until: Arc::default(),
            start: Instant::now(),
        }
    }

    fn sender(&self) -> Result<Sender, String> {
        Ok(Sender {
            enc: Encoder::new(false, SOUND_BITS)?,
            seq: 0,
            viewers: self.viewers.clone(),
            ducked_until: self.ducked_until.clone(),
            start: self.start,
            quiet: Vec::new(),
        })
    }

    fn add(&self, id: u64, conn: quinn::Connection) {
        let first = {
            let mut v = self.viewers.lock().unwrap();
            v.retain(|(i, _)| *i != id);
            v.push((id, conn));
            v.len() == 1
        };
        if first && Capture::system_sound_here() {
            let started = self.sender().and_then(|mut s| Capture::start(Source::SystemSound, move |f| s.send(f)));
            match started {
                Ok(c) => *self.capture.lock().unwrap() = Some(c),
                Err(e) => tracing::warn!(target: "annex::audio", "this computer's sound can't be captured: {e}"),
            }
        }
    }

    fn remove(&self, id: u64) {
        let last = {
            let mut v = self.viewers.lock().unwrap();
            let before = v.len();
            v.retain(|(i, _)| *i != id);
            before > 0 && v.is_empty()
        };
        if last {
            // Dropped outside the lock: stopping waits for the capture thread.
            let capture = self.capture.lock().unwrap().take();
            drop(capture);
            *self.pushed.lock().unwrap() = None;
        }
    }

    /// Whether anyone is listening (the Mac app captures sound only then).
    pub(crate) fn wanted(&self) -> bool {
        !self.viewers.lock().unwrap().is_empty()
    }

    /// Sound from the app (Mac): interleaved samples at any rate and channel count.
    pub(crate) fn push(&self, samples: &[f32], channels: u16, rate: u32) {
        if !self.wanted() || channels == 0 || rate == 0 {
            return;
        }
        let mut pushed = self.pushed.lock().unwrap();
        if pushed.as_ref().is_none_or(|p| p.rate != rate || p.channels != channels) {
            let sender = match self.sender() {
                Ok(s) => s,
                Err(e) => {
                    tracing::warn!(target: "annex::audio", "no sound encoder: {e}");
                    return;
                }
            };
            tracing::info!(target: "annex::audio", rate, channels, "this computer's sound comes from the app");
            *pushed = Some(Pushed { rate, channels, conv: Converter::new(rate, channels), sender });
        }
        let p = pushed.as_mut().unwrap();
        let Pushed { conv, sender, .. } = p;
        conv.push(samples, |f| sender.send(f));
    }

    fn duck(&self) {
        let until = self.start.elapsed().as_millis() as u64 + 400;
        self.ducked_until.store(until, Ordering::Relaxed);
    }
}

/// Sound for one session; it stops when dropped.
pub(crate) struct SessionAudio {
    task: tokio::task::JoinHandle<()>,
    core: Arc<Core>,
    id: u64,
}

impl Drop for SessionAudio {
    fn drop(&mut self) {
        self.task.abort();
        self.core.sound.remove(self.id);
    }
}

/// Where this side's incoming sound comes from: the host reads datagrams itself; the viewer's
/// datagram reader hands sound on.
pub(crate) enum Incoming {
    Conn,
    Handed(mpsc::UnboundedReceiver<Vec<u8>>),
}

/// Starts sound for a session. `host`: this computer is the one being viewed.
pub(crate) fn start(core: Arc<Core>, conn: quinn::Connection, id: u64, host: bool, incoming: Incoming) -> SessionAudio {
    let task = tokio::spawn(run(core.clone(), conn, id, host, incoming));
    SessionAudio { task, core, id }
}

async fn run(core: Arc<Core>, conn: quinn::Connection, id: u64, host: bool, mut incoming: Incoming) {
    let (expect, play_as) = if host {
        (AudioKind::Microphone, "the viewer's microphone")
    } else {
        (AudioKind::Sound, "the other computer's sound")
    };
    let mut tick = tokio::time::interval(Duration::from_millis(250));
    let mut sharing = false;
    let mut mic: Option<Capture> = None;
    let mut mic_failed = false;
    let mut want_output: Option<Output> = None;
    let mut player: Option<Player> = None;
    let mut player_failed = false;
    let mut decoder = Decoder::new().ok();
    let (mut received, mut bad) = (0u64, 0u64);
    let seq = Arc::new(AtomicU32::new(0));
    let start = Instant::now();
    loop {
        tokio::select! {
            _ = tick.tick() => {
                let s = core.audio();
                if host {
                    if s.share_sound != sharing {
                        sharing = s.share_sound;
                        tracing::info!(target: "annex::audio", session = id, on = sharing, "sharing this computer's sound");
                        if sharing { core.sound.add(id, conn.clone()) } else { core.sound.remove(id) }
                    }
                } else if s.send_mic != mic.is_some() && !(s.send_mic && mic_failed) {
                    if s.send_mic {
                        match start_mic(conn.clone(), seq.clone(), start) {
                            Ok(c) => {
                                tracing::info!(target: "annex::audio", "sending the microphone");
                                mic = Some(c);
                            }
                            Err(e) => {
                                tracing::warn!(target: "annex::audio", "the microphone can't be used: {e}");
                                mic_failed = true;
                            }
                        }
                    } else {
                        tracing::info!(target: "annex::audio", "microphone off");
                        mic = None;
                    }
                }
                if !s.send_mic {
                    mic_failed = false;
                }
                let want = match (s.hear_sound, host && s.virtual_mic && virtual_microphone_here()) {
                    (false, _) => None,
                    (true, true) => Some(Output::VirtualMicrophone),
                    (true, false) => Some(Output::Speakers),
                };
                if want != want_output {
                    tracing::info!(target: "annex::audio", output = ?want, "playing {play_as}");
                    want_output = want;
                    player = None;
                    player_failed = false;
                    // Annex Microphone is there for the whole session, so a call app can pick it
                    // before the viewer starts talking.
                    if want == Some(Output::VirtualMicrophone) {
                        match Player::start(Output::VirtualMicrophone) {
                            Ok(p) => player = Some(p),
                            Err(e) => {
                                tracing::warn!(target: "annex::audio", "can't add Annex Microphone: {e}");
                                player_failed = true;
                            }
                        }
                    }
                }
            }
            d = next(&conn, &mut incoming) => {
                let Some(d) = d else { break };
                let Ok((h, packet)) = AudioHeader::parse(&d) else { bad += 1; continue };
                if h.kind != expect {
                    bad += 1;
                    continue;
                }
                received += 1;
                let Some(output) = want_output else { continue };
                if player.is_none() && !player_failed {
                    match Player::start(output) {
                        Ok(p) => player = Some(p),
                        Err(e) => {
                            tracing::warn!(target: "annex::audio", ?output, "can't play {play_as}: {e}");
                            player_failed = true;
                        }
                    }
                }
                let (Some(p), Some(dec)) = (player.as_mut(), decoder.as_mut()) else { continue };
                if let Err(e) = dec.decode(h.seq, packet, |f| p.push(f)) {
                    tracing::debug!(target: "annex::audio", "bad sound packet: {e}");
                }
                if host && output == Output::Speakers {
                    core.sound.duck();
                }
            }
        }
    }
    tracing::debug!(target: "annex::audio", session = id, received, bad, "session sound ended");
}

async fn next(conn: &quinn::Connection, incoming: &mut Incoming) -> Option<Vec<u8>> {
    match incoming {
        Incoming::Conn => loop {
            let d = conn.read_datagram().await.ok()?;
            if annex_proto::audio::is_audio(&d) {
                return Some(d.to_vec());
            }
        },
        Incoming::Handed(rx) => rx.recv().await,
    }
}

fn start_mic(conn: quinn::Connection, seq: Arc<AtomicU32>, start: Instant) -> Result<Capture, String> {
    let mut enc = Encoder::new(true, MIC_BITS)?;
    Capture::start(Source::Microphone, move |frame| {
        let Ok(packet) = enc.encode(frame) else { return };
        let h = AudioHeader {
            seq: seq.fetch_add(1, Ordering::Relaxed),
            capture_ts_us: now_us(start),
            kind: AudioKind::Microphone,
        };
        let _ = conn.send_datagram(h.with(packet).into());
    })
}
