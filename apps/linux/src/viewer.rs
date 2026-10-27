//! The video window: decodes the other computer's screen with GStreamer and sends this
//! computer's mouse and keyboard back (docs/LINUX.md, "Viewing").
//!
//! Frames go straight from the core's thread into the decoder; only the window lives on the
//! main thread. ⌃⌥⇧Q ends the session and ⌃⌥⇧F switches full screen, like on the Mac and
//! Windows.

use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use annex_media::geometry::normalize_point;
use annex_session::{DisplayInfo, FrameOut, FrameSink, InputEvent, MouseButton, ViewerHandle, keys};
use gst::prelude::*;
use gtk::prelude::*;
use gtk::{gdk, glib};

use crate::model::Shared;

/// libavcodec first: with slice threads it's the quickest to the screen (12 ms at 1080p60 on
/// a Jetson AGX Thor, where NVDEC took 33 ms), and fast enough on a Raspberry Pi 5 (about
/// 275 fps at 1080p). Graphics-chip decoders are for systems without it. `ANNEX_DECODER`
/// names a decoder (and its options) to use instead.
const DECODERS: &[(&str, &str)] = &[
    ("avdec_h264", "avdec_h264 thread-type=slice"),
    ("vah264dec", "vah264dec"),
    ("nvh264dec", "nvh264dec max-display-delay=0"),
    // Jetson before JetPack 7: decodes into NVMM memory; nvvidconv brings it back.
    ("nvv4l2decoder", "nvv4l2decoder disable-dpb=true ! nvvidconv"),
    ("v4l2slh264dec", "v4l2slh264dec"),
    ("v4l2h264dec", "v4l2h264dec"),
    ("openh264dec", "openh264dec"),
];

pub fn decoder() -> Option<String> {
    if let Ok(name) = std::env::var("ANNEX_DECODER") {
        return Some(name);
    }
    DECODERS.iter().find(|(f, _)| gst::ElementFactory::find(f).is_some()).map(|(_, d)| d.to_string())
}

struct Decoder {
    pipeline: gst::Pipeline,
    paintable: gdk::Paintable,
}

impl Decoder {
    fn new(handle: Arc<ViewerHandle>) -> Result<(Decoder, FrameSink), String> {
        let dec = decoder().ok_or("No H.264 decoder found. Install gstreamer1.0-libav.")?;
        tracing::info!("viewer decoder: {dec}");
        let desc = format!(
            "appsrc name=src is-live=true format=time \
               caps=video/x-h264,stream-format=byte-stream,alignment=au \
             ! h264parse ! {dec} ! videoconvert ! gtk4paintablesink name=sink sync=false"
        );
        let pipeline = gst::parse::launch(&desc)
            .map_err(|e| e.to_string())?
            .downcast::<gst::Pipeline>()
            .map_err(|_| "not a pipeline")?;
        let src = pipeline.by_name("src").and_then(|e| e.downcast::<gst_app::AppSrc>().ok()).ok_or("no appsrc")?;
        let sink = pipeline.by_name("sink").ok_or("no sink")?;
        let paintable = sink.property::<gdk::Paintable>("paintable");

        // A frame counts as shown once the sink takes it; the core times latency from that.
        if let Some(pad) = sink.static_pad("sink") {
            let h = handle.clone();
            pad.add_probe(gst::PadProbeType::BUFFER, move |_, info| {
                if let Some(pts) = info.buffer().and_then(|b| b.pts()) {
                    h.frame_presented((pts.mseconds() & 0xffff_ffff) as u32);
                }
                gst::PadProbeReturn::Ok
            });
        }

        // Ask for a fresh keyframe whenever the decoder trips over a lost frame.
        let bus = pipeline.bus().ok_or("no bus")?;
        let h = handle.clone();
        bus.set_sync_handler(move |_, msg| {
            if let gst::MessageView::Error(e) = msg.view() {
                tracing::warn!("decoder: {} ({:?})", e.error(), e.debug());
                h.request_keyframe();
            }
            gst::BusSyncReply::Drop
        });

        if let Err(e) = pipeline.set_state(gst::State::Playing) {
            let _ = pipeline.set_state(gst::State::Null);
            return Err(e.to_string());
        }
        handle.request_keyframe();

        let started = Arc::new(AtomicBool::new(false));
        let frames: FrameSink = Arc::new(move |f: FrameOut<'_>| {
            // A stream can only start at a keyframe.
            if !started.load(Ordering::Relaxed) {
                if !f.keyframe {
                    return;
                }
                started.store(true, Ordering::Relaxed);
            }
            let mut buf = gst::Buffer::from_mut_slice(f.data.to_vec());
            {
                let b = buf.get_mut().unwrap();
                // The frame id rides in the timestamp (as milliseconds) to the sink's probe.
                b.set_pts(gst::ClockTime::from_mseconds(f.frame_id.into()));
                if !f.keyframe {
                    b.set_flags(gst::BufferFlags::DELTA_UNIT);
                }
            }
            let _ = src.push_buffer(buf);
        });
        Ok((Decoder { pipeline, paintable }, frames))
    }
}

impl Drop for Decoder {
    fn drop(&mut self) {
        let _ = self.pipeline.set_state(gst::State::Null);
    }
}

struct VideoWindow {
    window: gtk::ApplicationWindow,
    _decoder: Decoder,
    size: Rc<RefCell<(u32, u32)>>,
}

thread_local! {
    static WINDOW: RefCell<Option<VideoWindow>> = const { RefCell::new(None) };
}

/// This computer's screen, for the other computer (Extend makes a display this size).
pub fn display_info(widget: Option<&impl IsA<gtk::Widget>>) -> DisplayInfo {
    let display = gdk::Display::default();
    let monitor = display.as_ref().and_then(|d| {
        widget
            .and_then(|w| w.native())
            .and_then(|n| n.surface())
            .and_then(|s| d.monitor_at_surface(&s))
            .or_else(|| d.monitors().item(0).and_then(|m| m.downcast::<gdk::Monitor>().ok()))
    });
    match monitor {
        Some(m) => {
            let g = m.geometry();
            let scale = m.scale_factor().max(1) as u32;
            let hz = ((m.refresh_rate() + 500) / 1000) as u32;
            DisplayInfo {
                width_px: g.width() as u32 * scale,
                height_px: g.height() as u32 * scale,
                width_mm: m.width_mm().max(0) as u32,
                height_mm: m.height_mm().max(0) as u32,
                refresh_hz: if hz == 0 { 60 } else { hz },
                scale_percent: scale * 100,
                refresh_rates: vec![],
            }
        }
        None => DisplayInfo {
            width_px: 1920,
            height_px: 1080,
            width_mm: 0,
            height_mm: 0,
            refresh_hz: 60,
            scale_percent: 100,
            refresh_rates: vec![],
        },
    }
}

/// Opens the window for the session the model just started.
pub fn open(app: &gtk::Application, model: &Shared) {
    close();
    let (handle, target, title, size, full_screen) = {
        let m = model.borrow();
        let Some(v) = &m.viewer else { return };
        let title = match m.role() {
            Some(crate::model::SessionRole::Viewing { extend: true, .. }) => {
                format!("Extra display from {} — Annex", m.label(&m.viewer_peer))
            }
            _ => format!("{} — Annex", m.label(&m.viewer_peer)),
        };
        (v.handle.clone(), v.frames.clone(), title, m.stream_size.unwrap_or((1920, 1080)), m.settings.open_full_screen)
    };
    let (decoder, frames) = match Decoder::new(handle.clone()) {
        Ok(d) => d,
        Err(e) => {
            tracing::error!("can't show the stream: {e}");
            let mut m = model.borrow_mut();
            m.disconnect();
            m.viewer_status = Some(format!("Can't show the stream: {e}"));
            drop(m);
            crate::ui::apply(app, model);
            return;
        }
    };
    *target.lock().unwrap() = Some(frames);

    let picture = gtk::Picture::for_paintable(&decoder.paintable);
    picture.set_keep_aspect_ratio(true);
    picture.set_can_shrink(true);
    picture.set_hexpand(true);
    picture.set_vexpand(true);
    picture.add_css_class("video");
    picture.set_focusable(true);

    let window = gtk::ApplicationWindow::builder().application(app).title(title).child(&picture).build();
    window.add_css_class("video-window");
    // Most of the screen, at the stream's shape.
    let (sw, sh) = size;
    let d = display_info(None::<&gtk::Widget>);
    let scale = d.scale_percent.max(100) as f64 / 100.0;
    let (mw, mh) = (d.width_px as f64 / scale * 0.8, d.height_px as f64 / scale * 0.8);
    let k = (mw / sw as f64).min(mh / sh as f64).min(1.0);
    window.set_default_size((sw as f64 * k) as i32, (sh as f64 * k) as i32);

    let size = Rc::new(RefCell::new(size));
    add_input(&window, &picture, handle, model.clone(), size.clone(), app.clone());

    let (m2, app2) = (model.clone(), app.clone());
    window.connect_close_request(move |_| {
        // Closing the window ends the session (the model then drops this window).
        glib::idle_add_local_once({
            let (m, a) = (m2.clone(), app2.clone());
            move || {
                m.borrow_mut().disconnect();
                crate::ui::apply(&a, &m);
            }
        });
        glib::Propagation::Proceed
    });

    if full_screen {
        window.fullscreen();
    }
    window.present();
    picture.grab_focus();
    WINDOW.with(|w| *w.borrow_mut() = Some(VideoWindow { window, _decoder: decoder, size }));
}

pub fn close() {
    if let Some(v) = WINDOW.with(|w| w.borrow_mut().take()) {
        v.window.destroy();
    }
}

pub fn stream_size(width: u32, height: u32) {
    WINDOW.with(|w| {
        if let Some(v) = w.borrow().as_ref() {
            *v.size.borrow_mut() = (width, height);
        }
    });
}

fn add_input(
    window: &gtk::ApplicationWindow,
    picture: &gtk::Picture,
    handle: Arc<ViewerHandle>,
    model: Shared,
    size: Rc<RefCell<(u32, u32)>>,
    app: gtk::Application,
) {
    let controlling = {
        let m = model.clone();
        move || m.try_borrow().map(|m| m.controlling).unwrap_or(false)
    };

    // Keys: everything goes to the other computer while controlling it, except ⌃⌥⇧Q and ⌃⌥⇧F.
    let keys_down: Rc<RefCell<HashSet<u16>>> = Rc::default();
    let key = gtk::EventControllerKey::new();
    key.set_propagation_phase(gtk::PropagationPhase::Capture);
    {
        let (h, down, ctl, w, m, a) =
            (handle.clone(), keys_down.clone(), controlling.clone(), window.clone(), model.clone(), app.clone());
        key.connect_key_pressed(move |_, keyval, keycode, state| {
            let chord = gdk::ModifierType::CONTROL_MASK | gdk::ModifierType::ALT_MASK | gdk::ModifierType::SHIFT_MASK;
            if state.contains(chord) {
                match keyval.to_lower() {
                    gdk::Key::q => {
                        let (m, a) = (m.clone(), a.clone());
                        glib::idle_add_local_once(move || {
                            m.borrow_mut().disconnect();
                            crate::ui::apply(&a, &m);
                        });
                        return glib::Propagation::Stop;
                    }
                    gdk::Key::f => {
                        if w.is_fullscreen() {
                            w.unfullscreen()
                        } else {
                            w.fullscreen()
                        }
                        return glib::Propagation::Stop;
                    }
                    _ => {}
                }
            }
            if !ctl() {
                return glib::Propagation::Proceed;
            }
            // X keycodes are evdev codes + 8.
            if let Some(usage) = keycode.checked_sub(8).and_then(|c| keys::hid_from_linux(c as u16)) {
                // The other computer repeats held keys itself.
                if down.borrow_mut().insert(usage) {
                    h.send_input(InputEvent::Key { usage, down: true });
                }
            }
            glib::Propagation::Stop
        });
    }
    {
        let (h, down, ctl) = (handle.clone(), keys_down.clone(), controlling.clone());
        key.connect_key_released(move |_, _, keycode, _| {
            if let Some(usage) = keycode.checked_sub(8).and_then(|c| keys::hid_from_linux(c as u16))
                && down.borrow_mut().remove(&usage)
                && ctl()
            {
                h.send_input(InputEvent::Key { usage, down: false });
            }
        });
    }
    window.add_controller(key);

    // Leaving the window lets go of every key, so none stays held on the other computer.
    {
        let (h, down) = (handle.clone(), keys_down.clone());
        let (model, app) = (model.clone(), app.clone());
        window.connect_is_active_notify(move |w| {
            crate::ui::act(&app, &model, |m| m.viewer_focus(w.is_active()));
            if !w.is_active() {
                for usage in down.borrow_mut().drain() {
                    h.send_input(InputEvent::Key { usage, down: false });
                }
            }
        });
    }

    // Keep system shortcuts (Super, Alt+Tab) for the other computer while controlling it.
    {
        let ctl = controlling.clone();
        window.connect_realize(move |w| {
            if ctl()
                && let Some(t) = w.surface().and_then(|s| s.downcast::<gdk::Toplevel>().ok())
            {
                t.inhibit_system_shortcuts(None::<&gdk::Event>);
            }
        });
    }

    let to_stream = {
        let (p, size) = (picture.clone(), size.clone());
        move |x: f64, y: f64| {
            let (vw, vh) = *size.borrow();
            normalize_point(x as i32, y as i32, vw, vh, p.width().max(1) as u32, p.height().max(1) as u32)
        }
    };

    let motion = gtk::EventControllerMotion::new();
    {
        let (h, ctl, f, p) = (handle.clone(), controlling.clone(), to_stream.clone(), picture.clone());
        motion.connect_motion(move |_, x, y| {
            if ctl() {
                p.set_cursor_from_name(Some("none"));
                let (x, y) = f(x, y);
                h.send_input(InputEvent::MouseMove { x, y });
            } else {
                p.set_cursor(None);
            }
        });
    }
    picture.add_controller(motion);

    let click = gtk::GestureClick::new();
    click.set_button(0);
    {
        let (h, ctl, f) = (handle.clone(), controlling.clone(), to_stream.clone());
        click.connect_pressed(move |g, _, x, y| {
            if let Some(button) = mouse_button(g.current_button())
                && ctl()
            {
                let (x, y) = f(x, y);
                h.send_input(InputEvent::MouseMove { x, y });
                h.send_input(InputEvent::MouseButton { button, down: true });
            }
        });
    }
    {
        let (h, ctl) = (handle.clone(), controlling.clone());
        click.connect_released(move |g, _, _, _| {
            if let Some(button) = mouse_button(g.current_button())
                && ctl()
            {
                h.send_input(InputEvent::MouseButton { button, down: false });
            }
        });
    }
    picture.add_controller(click);

    let scroll = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::BOTH_AXES);
    {
        let (h, ctl) = (handle, controlling);
        scroll.connect_scroll(move |_, dx, dy| {
            if !ctl() {
                return glib::Propagation::Proceed;
            }
            // GTK counts wheel clicks (fractions on a touchpad); the protocol counts pixels,
            // positive when the content moves down or right.
            const STEP: f64 = 40.0;
            h.send_input(InputEvent::Scroll { dx: (-dx * STEP).round() as i32, dy: (-dy * STEP).round() as i32 });
            glib::Propagation::Stop
        });
    }
    picture.add_controller(scroll);
}

fn mouse_button(b: u32) -> Option<MouseButton> {
    match b {
        1 => Some(MouseButton::Left),
        2 => Some(MouseButton::Middle),
        3 => Some(MouseButton::Right),
        _ => None,
    }
}
