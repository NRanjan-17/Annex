//! Captures one named output on wlroots compositors (labwc, sway) with `wlr-screencopy`: no
//! portal dialog, and it can capture the headless output Extend adds.

use std::os::fd::AsFd;
use std::os::unix::fs::FileExt;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use wayland_client::Proxy as _;
use wayland_client::protocol::{wl_buffer, wl_output, wl_shm};

use crate::wl::{FrameInfo, Wayland};

pub struct Screencopy {
    wl: Wayland,
    output: wl_output::WlOutput,
    info: FrameInfo,
    file: std::fs::File,
    buffer: wl_buffer::WlBuffer,
}

impl Screencopy {
    /// Connects and learns the output's frame format with one capture.
    pub fn open(output: &str) -> Result<Screencopy, String> {
        let mut wl = Wayland::connect()?;
        if wl.state.screencopy.is_none() {
            return Err("this desktop has no screencopy".into());
        }
        let out = wl.output(output).ok_or_else(|| format!("no output called {output}"))?.output.clone();
        let info = frame_info(&mut wl, &out)?;
        let size = u64::from(info.stride) * u64::from(info.height);
        let file = crate::wl::shm_file(size).map_err(|e| e.to_string())?;
        let qh = wl.qh();
        let shm = wl.state.shm.clone().ok_or("no wl_shm")?;
        let pool = shm.create_pool(file.as_fd(), size as i32, &qh, ());
        let buffer = pool.create_buffer(
            0,
            info.width as i32,
            info.height as i32,
            info.stride as i32,
            info.format.ok_or("no frame format")?,
            &qh,
            (),
        );
        pool.destroy();
        tracing::info!(output, width = info.width, height = info.height, format = ?info.format, "screencopy: ready");
        Ok(Screencopy { wl, output: out, info, file, buffer })
    }

    pub fn size(&self) -> (u32, u32) {
        (self.info.width, self.info.height)
    }

    /// The raw-video caps of the frames this pushes.
    pub fn caps(&self) -> String {
        let format = match self.info.format {
            Some(wl_shm::Format::Xbgr8888 | wl_shm::Format::Abgr8888) => "RGBx",
            _ => "BGRx",
        };
        format!("video/x-raw,format={format},width={},height={},framerate=0/1", self.info.width, self.info.height)
    }

    /// Captures until `stop`, at most `fps` frames a second, into `src`. Returns when the
    /// output changes size (the caller starts again at the new size) or goes away.
    pub fn run(mut self, src: gst_app::AppSrc, fps: u32, stop: Arc<AtomicBool>) {
        let period = Duration::from_micros(1_000_000 / u64::from(fps.max(1)));
        let (w, h, stride) = (self.info.width as usize, self.info.height as usize, self.info.stride as usize);
        let mut raw = vec![0u8; stride * h];
        while !stop.load(Ordering::Acquire) {
            let started = Instant::now();
            let qh = self.wl.qh();
            let Some(mgr) = self.wl.state.screencopy.clone() else {
                tracing::warn!("screencopy: the desktop took it away");
                return;
            };
            self.wl.state.frame = FrameInfo::default();
            let frame = mgr.capture_output(1, &self.output, &qh, ());
            // Wait for the description, then copy into our buffer.
            while !(self.wl.state.frame.buffer_done || self.wl.state.frame.failed)
                && !(self.wl.state.frame.format.is_some() && mgr.version() < 3)
            {
                if let Err(e) = self.wl.dispatch() {
                    tracing::warn!("screencopy: lost the desktop: {e}");
                    return;
                }
            }
            let f = self.wl.state.frame.clone();
            if f.failed || (f.width, f.height) != (self.info.width, self.info.height) {
                frame.destroy();
                tracing::info!("screencopy: the output changed or went away");
                return;
            }
            frame.copy(&self.buffer);
            while !(self.wl.state.frame.ready || self.wl.state.frame.failed) {
                if let Err(e) = self.wl.dispatch() {
                    tracing::warn!("screencopy: lost the desktop: {e}");
                    return;
                }
            }
            frame.destroy();
            if self.wl.state.frame.ready && self.file.read_exact_at(&mut raw, 0).is_ok() {
                // Tightly packed rows, top first.
                let mut packed = vec![0u8; w * 4 * h];
                for row in 0..h {
                    let from = if f.y_invert { h - 1 - row } else { row };
                    packed[row * w * 4..(row + 1) * w * 4].copy_from_slice(&raw[from * stride..from * stride + w * 4]);
                }
                if let Err(e) = src.push_buffer(gst::Buffer::from_mut_slice(packed)) {
                    tracing::info!("screencopy: the encoder stopped taking frames ({e:?})");
                    return;
                }
            }
            if let Some(rest) = period.checked_sub(started.elapsed()) {
                std::thread::sleep(rest);
            }
        }
        self.buffer.destroy();
        tracing::debug!("screencopy: stopped");
    }
}

fn frame_info(wl: &mut Wayland, output: &wl_output::WlOutput) -> Result<FrameInfo, String> {
    let qh = wl.qh();
    let mgr = wl.state.screencopy.clone().ok_or("no screencopy")?;
    wl.state.frame = FrameInfo::default();
    let frame = mgr.capture_output(1, output, &qh, ());
    loop {
        let f = &wl.state.frame;
        if f.failed {
            frame.destroy();
            return Err("the desktop refused to capture the screen".into());
        }
        if f.buffer_done || (f.format.is_some() && mgr.version() < 3) {
            break;
        }
        wl.dispatch()?;
    }
    frame.destroy();
    let info = wl.state.frame.clone();
    if info.format.is_none() {
        return Err("the desktop offered no shared-memory frame format".into());
    }
    Ok(info)
}
