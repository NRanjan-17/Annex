//! A Wayland connection of Annex's own, for what wlroots compositors (labwc, sway) offer
//! without a portal: screencopy, a virtual pointer and keyboard, and output names.

use wayland_client::protocol::{wl_buffer, wl_output, wl_registry, wl_seat, wl_shm, wl_shm_pool};
use wayland_client::{Connection, Dispatch, EventQueue, QueueHandle, WEnum, delegate_noop};
use wayland_protocols_misc::zwp_virtual_keyboard_v1::client::{
    zwp_virtual_keyboard_manager_v1::ZwpVirtualKeyboardManagerV1, zwp_virtual_keyboard_v1::ZwpVirtualKeyboardV1,
};
use wayland_protocols_wlr::screencopy::v1::client::{
    zwlr_screencopy_frame_v1::{self, ZwlrScreencopyFrameV1},
    zwlr_screencopy_manager_v1::ZwlrScreencopyManagerV1,
};
use wayland_protocols_wlr::virtual_pointer::v1::client::{
    zwlr_virtual_pointer_manager_v1::ZwlrVirtualPointerManagerV1, zwlr_virtual_pointer_v1::ZwlrVirtualPointerV1,
};

pub struct Output {
    pub global: u32,
    pub output: wl_output::WlOutput,
    pub name: String,
    /// The current mode, in pixels.
    pub size: (i32, i32),
}

/// What one screencopy frame reported.
#[derive(Default, Debug, Clone)]
pub struct FrameInfo {
    pub format: Option<wl_shm::Format>,
    pub width: u32,
    pub height: u32,
    pub stride: u32,
    /// The compositor described every buffer type it offers (version 3), or sent `buffer`.
    pub buffer_done: bool,
    pub ready: bool,
    pub failed: bool,
    pub y_invert: bool,
}

#[derive(Default)]
pub struct State {
    pub outputs: Vec<Output>,
    pub seat: Option<wl_seat::WlSeat>,
    pub shm: Option<wl_shm::WlShm>,
    pub screencopy: Option<ZwlrScreencopyManagerV1>,
    pub pointer_manager: Option<ZwlrVirtualPointerManagerV1>,
    pub keyboard_manager: Option<ZwpVirtualKeyboardManagerV1>,
    pub frame: FrameInfo,
}

pub struct Wayland {
    pub conn: Connection,
    pub queue: EventQueue<State>,
    pub state: State,
}

impl Wayland {
    pub fn connect() -> Result<Wayland, String> {
        let conn = Connection::connect_to_env().map_err(|e| format!("no Wayland display: {e}"))?;
        let mut queue = conn.new_event_queue();
        let qh = queue.handle();
        conn.display().get_registry(&qh, ());
        let mut state = State::default();
        // Globals, then the outputs' names and modes.
        queue.roundtrip(&mut state).map_err(|e| e.to_string())?;
        queue.roundtrip(&mut state).map_err(|e| e.to_string())?;
        Ok(Wayland { conn, queue, state })
    }

    pub fn qh(&self) -> QueueHandle<State> {
        self.queue.handle()
    }

    pub fn roundtrip(&mut self) -> Result<(), String> {
        self.queue.roundtrip(&mut self.state).map(|_| ()).map_err(|e| e.to_string())
    }

    pub fn dispatch(&mut self) -> Result<(), String> {
        self.queue.blocking_dispatch(&mut self.state).map(|_| ()).map_err(|e| e.to_string())
    }

    pub fn output(&self, name: &str) -> Option<&Output> {
        self.state.outputs.iter().find(|o| o.name == name)
    }
}

impl Dispatch<wl_registry::WlRegistry, ()> for State {
    fn event(
        state: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        match event {
            wl_registry::Event::Global { name, interface, version } => match interface.as_str() {
                "wl_output" => {
                    let output = registry.bind::<wl_output::WlOutput, _, _>(name, version.min(4), qh, name);
                    state.outputs.push(Output { global: name, output, name: String::new(), size: (0, 0) });
                }
                "wl_seat" if state.seat.is_none() => {
                    state.seat = Some(registry.bind(name, version.min(5), qh, ()));
                }
                "wl_shm" => state.shm = Some(registry.bind(name, 1, qh, ())),
                "zwlr_screencopy_manager_v1" => state.screencopy = Some(registry.bind(name, version.min(3), qh, ())),
                "zwlr_virtual_pointer_manager_v1" => {
                    state.pointer_manager = Some(registry.bind(name, version.min(2), qh, ()))
                }
                "zwp_virtual_keyboard_manager_v1" => state.keyboard_manager = Some(registry.bind(name, 1, qh, ())),
                _ => {}
            },
            wl_registry::Event::GlobalRemove { name } => state.outputs.retain(|o| o.global != name),
            _ => {}
        }
    }
}

impl Dispatch<wl_output::WlOutput, u32> for State {
    fn event(
        state: &mut Self,
        _: &wl_output::WlOutput,
        event: wl_output::Event,
        global: &u32,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let Some(o) = state.outputs.iter_mut().find(|o| o.global == *global) else { return };
        match event {
            wl_output::Event::Name { name } => o.name = name,
            wl_output::Event::Mode { flags: WEnum::Value(f), width, height, .. }
                if f.contains(wl_output::Mode::Current) =>
            {
                o.size = (width, height)
            }
            _ => {}
        }
    }
}

impl Dispatch<ZwlrScreencopyFrameV1, ()> for State {
    fn event(
        state: &mut Self,
        _: &ZwlrScreencopyFrameV1,
        event: zwlr_screencopy_frame_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        use zwlr_screencopy_frame_v1::Event;
        let f = &mut state.frame;
        match event {
            Event::Buffer { format, width, height, stride } => {
                // The first shm format offered is the one to use.
                if f.format.is_none()
                    && let WEnum::Value(format) = format
                {
                    f.format = Some(format);
                    (f.width, f.height, f.stride) = (width, height, stride);
                }
            }
            Event::Flags { flags: WEnum::Value(flags) } => {
                f.y_invert = flags.contains(zwlr_screencopy_frame_v1::Flags::YInvert)
            }
            Event::BufferDone => f.buffer_done = true,
            Event::Ready { .. } => f.ready = true,
            Event::Failed => f.failed = true,
            _ => {}
        }
    }
}

delegate_noop!(State: ignore wl_seat::WlSeat);
delegate_noop!(State: ignore wl_shm::WlShm);
delegate_noop!(State: ignore wl_shm_pool::WlShmPool);
delegate_noop!(State: ignore wl_buffer::WlBuffer);
delegate_noop!(State: ZwlrScreencopyManagerV1);
delegate_noop!(State: ZwlrVirtualPointerManagerV1);
delegate_noop!(State: ignore ZwlrVirtualPointerV1);
delegate_noop!(State: ZwpVirtualKeyboardManagerV1);
delegate_noop!(State: ignore ZwpVirtualKeyboardV1);

/// An unlinked file in `$XDG_RUNTIME_DIR` (a tmpfs) of `size` bytes, for shm buffers and keymaps.
pub fn shm_file(size: u64) -> std::io::Result<std::fs::File> {
    let dir = std::env::var_os("XDG_RUNTIME_DIR").map(std::path::PathBuf::from).unwrap_or_else(std::env::temp_dir);
    for i in 0..100u32 {
        let path = dir.join(format!("annex-shm-{}-{i}", std::process::id()));
        match std::fs::OpenOptions::new().read(true).write(true).create_new(true).open(&path) {
            Ok(f) => {
                let _ = std::fs::remove_file(&path);
                f.set_len(size)?;
                return Ok(f);
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Err(std::io::Error::other("no free shm file name"))
}
