//! Annex for Linux: show another computer's screen, share this one, or use it as an extra
//! display. GTK 4 for the window, GStreamer for video, the shared Rust core for the rest
//! (docs/LINUX.md).

mod cli;
mod clipboard;
mod desktop;
mod drop_tray;
mod encode;
mod extend;
mod gnome;
mod host;
mod inject;
mod layer;
mod model;
mod portal;
mod remote;
mod screencopy;
mod settings;
mod settings_keys;
mod tray;
mod ui;
mod viewer;
mod wl;
mod x11_drag;

use gtk::prelude::*;
use gtk::{gdk, gio, glib};

use crate::model::Model;

const APP_ID: &str = "dev.getannex.Annex";

fn main() -> glib::ExitCode {
    let args: Vec<String> = std::env::args().collect();
    let opts = match cli::parse(&args[1..]) {
        Ok(cli::Parsed::Launch(opts)) => opts,
        Ok(cli::Parsed::Help(topic)) => {
            return match remote::help(topic.as_deref()) {
                Ok(text) => {
                    print!("{text}");
                    glib::ExitCode::SUCCESS
                }
                Err(e) => {
                    eprintln!("{e}");
                    glib::ExitCode::from(2)
                }
            };
        }
        Ok(cli::Parsed::Version) => {
            println!("{}", remote::version_line());
            return glib::ExitCode::SUCCESS;
        }
        // `annex <command>`: to the running Annex, before any of the app starts here.
        Ok(cli::Parsed::Run(cmd)) => return remote::client(&args, cmd),
        Err(cli::Usage(e)) => {
            eprintln!("{e}");
            return glib::ExitCode::from(2);
        }
    };

    // Before the log and the identity exist: an update, or a first install?
    let used_before = settings::used_before();
    // The log first, so everything below is in it (stderr too, for a terminal).
    annex_log::init(&settings::data_dir(), true);
    tracing::info!(
        background = opts.background,
        share = opts.share,
        headless = opts.headless,
        connect = opts.connect.is_some(),
        extend = opts.extend,
        "app: starting"
    );

    // Before GTK and libwayland load.
    layer::preload();

    if let Err(e) = gst::init() {
        tracing::error!("app: GStreamer didn't start: {e}");
        eprintln!("Annex needs GStreamer 1.20 or later: {e}");
        return glib::ExitCode::FAILURE;
    }
    // The video sink is built in, so no GStreamer GTK plugin package is needed.
    if let Err(e) = gstgtk4::plugin_register_static() {
        tracing::error!("app: couldn't register the GTK video sink: {e}");
    }

    // HANDLES_COMMAND_LINE: a second `annex …` hands its whole command line to this one
    // (remote.rs), which answers in that terminal.
    let app =
        gtk::Application::builder().application_id(APP_ID).flags(gio::ApplicationFlags::HANDLES_COMMAND_LINE).build();
    let started = std::rc::Rc::new(std::cell::Cell::new(false));
    {
        let started = started.clone();
        app.connect_command_line(move |app, cmdline| {
            let args: Vec<String> = cmdline.arguments().iter().map(|a| a.to_string_lossy().into_owned()).collect();
            match cli::parse(args.get(1..).unwrap_or_default()) {
                Ok(cli::Parsed::Launch(opts)) => {
                    launch(app, opts, &started, used_before);
                    glib::ExitCode::SUCCESS
                }
                Ok(cli::Parsed::Run(cmd)) => remote::run(app, cmdline, MODEL.with(|m| m.borrow().clone()), cmd),
                other => remote::reply(cmdline, other),
            }
        });
    }
    // The desktop asking for Annex (its own launcher, a notification): the window.
    app.connect_activate(move |app| launch(app, cli::LaunchOpts::default(), &started, used_before));
    // GLib leaves options it doesn't know for the command-line handler.
    app.run_with_args(&args)
}

/// Annex started, or was started again (`annex`, `annex --share`, the desktop's launcher): the
/// options apply to the running one, and the window shows unless `--background`.
fn launch(app: &gtk::Application, opts: cli::LaunchOpts, started: &std::cell::Cell<bool>, used_before: bool) {
    if !started.replace(true) {
        start(app, opts, used_before);
        return;
    }
    tracing::info!(
        background = opts.background,
        share = opts.share,
        headless = opts.headless,
        connect = opts.connect.is_some(),
        "app: launched again"
    );
    let Some(model) = MODEL.with(|m| m.borrow().clone()) else { return };
    if opts.headless {
        turn_on_headless(&model);
    }
    if opts.share {
        ui::act(app, &model, |m| m.set_hosting(true));
    }
    if let Some(to) = opts.connect {
        connect_at_launch(app, &model, &to, opts.extend);
    }
    if !opts.background {
        ui::present(app, &model);
    }
}

thread_local! {
    static MODEL: std::cell::RefCell<Option<model::Shared>> = const { std::cell::RefCell::new(None) };
}

fn start(app: &gtk::Application, opts: cli::LaunchOpts, used_before: bool) {
    ui::load_css();
    // Keep running with no window open: Annex lives in the tray.
    std::mem::forget(app.hold());

    let (tx, rx) = async_channel::unbounded();
    let model = Model::new(tx);
    MODEL.with(|m| *m.borrow_mut() = Some(model.clone()));
    log_boot(&model.borrow());

    // The core's events, on the main thread.
    {
        let (app, model) = (app.clone(), model.clone());
        glib::spawn_future_local(async move {
            while let Ok(msg) = rx.recv().await {
                ui::act(&app, &model, |m| m.handle(msg));
            }
        });
    }

    let (tray_tx, tray_rx) = async_channel::unbounded();
    let has_tray = tray::start(&model.borrow(), tray_tx);
    {
        let (app, model) = (app.clone(), model.clone());
        glib::spawn_future_local(async move {
            while let Ok(a) = tray_rx.recv().await {
                tracing::info!(target: "annex::tray", "tray: {}", a.describe());
                match a {
                    tray::Action::Show => ui::present(&app, &model),
                    tray::Action::Settings => ui::show_pane(&app, &model, "settings"),
                    tray::Action::Share(on) => ui::act(&app, &model, |m| m.set_hosting(on)),
                    tray::Action::Connect { peer, extend } => {
                        let display = viewer::display_info(None::<&gtk::Widget>);
                        ui::act(&app, &model, |m| m.connect_peer(&peer, extend, display))
                    }
                    tray::Action::Disconnect => ui::act(&app, &model, |m| m.disconnect()),
                    tray::Action::Quit => quit(&app, &model),
                }
            }
        });
    }

    let quit_action = gio::SimpleAction::new("quit", None);
    {
        let (app2, model) = (app.clone(), model.clone());
        quit_action.connect_activate(move |_, _| quit(&app2, &model));
    }
    app.add_action(&quit_action);
    app.set_accels_for_action("app.quit", &["<Control>q"]);

    let share_clipboard = model.borrow().settings.share_clipboard;
    model.borrow_mut().set_clipboard_sharing(share_clipboard);
    model.borrow_mut().start_drop_tray();
    if opts.headless {
        turn_on_headless(&model);
    }
    if let Some(file) = model.borrow().core.pin_file().filter(|_| model.borrow().settings.headless) {
        tracing::info!("headless: pairing PINs appear in {}", file.display());
    }
    if opts.share || model.borrow().settings.start_sharing_at_launch {
        tracing::info!(target: "annex::host", from_option = opts.share, "sharing: on at launch");
        ui::act(app, &model, |m| m.set_hosting(true));
    }
    // Updates: now, then once a day, while Settings → Check for updates is on.
    if model.borrow().settings.check_for_updates {
        ui::act(app, &model, |m| m.check_for_updates());
    }
    {
        let (app, model) = (app.clone(), model.clone());
        glib::timeout_add_seconds_local(24 * 60 * 60, move || {
            if model.borrow().settings.check_for_updates {
                ui::act(&app, &model, |m| m.check_for_updates());
            }
            glib::ControlFlow::Continue
        });
    }
    // Shown with the window: now, or when it first opens from the tray.
    ui::whats_new_at_launch(&model, used_before);
    // Without a tray there is no other way back to Annex, so the window always opens then.
    if !opts.background || !has_tray {
        ui::present(app, &model);
    } else {
        tracing::info!("app: started in the tray, without the window");
    }
    if let Some(to) = opts.connect {
        connect_at_launch(app, &model, &to, opts.extend);
    }
}

/// `--headless` (or `annex start --headless`): turns on, and keeps, No one at this screen.
fn turn_on_headless(model: &model::Shared) {
    if model.borrow().settings.headless {
        return;
    }
    tracing::info!(target: "annex::settings", "settings: --headless turns on No one at this screen");
    let mut m = model.borrow_mut();
    m.settings.headless = true;
    m.core.set_headless(true);
    m.save_settings();
}

/// `--connect <address | code> [--extend]`.
fn connect_at_launch(app: &gtk::Application, model: &model::Shared, to: &str, extend: bool) {
    tracing::info!(target: "annex::viewer", extend, "viewer: connecting at launch (--connect)");
    let display = viewer::display_info(None::<&gtk::Widget>);
    ui::act(app, model, |m| {
        if ui::looks_like_code(to) {
            m.connect_code(to, extend, display)
        } else {
            m.connect_address(to, extend, display)
        }
    });
}

fn quit(app: &gtk::Application, model: &model::Shared) {
    tracing::info!("app: quitting");
    model.borrow_mut().quit();
    viewer::close();
    app.quit();
}

/// What this computer is, for the top of the log: the desktop, how Annex works on it, and the
/// versions of what it's built on.
fn log_boot(m: &Model) {
    let session = desktop::session();
    let display = gdk::Display::default();
    let backend = match &display {
        Some(d) if d.downcast_ref::<gdk4_x11::X11Display>().is_some() => "X11",
        Some(_) => "Wayland",
        None => "no display",
    };
    let island = if layer::get().is_some() {
        "layer shell"
    } else if session == desktop::Session::X11 {
        "X11 window"
    } else {
        "none (files go on the viewer window)"
    };
    let os = std::fs::read_to_string("/etc/os-release")
        .ok()
        .and_then(|s| s.lines().find_map(|l| l.strip_prefix("PRETTY_NAME=").map(|v| v.trim_matches('"').to_string())))
        .unwrap_or_else(|| "unknown".into());
    let kernel =
        std::fs::read_to_string("/proc/sys/kernel/osrelease").map(|s| s.trim().to_string()).unwrap_or_default();
    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        os,
        kernel,
        desktop = std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default(),
        session_type = std::env::var("XDG_SESSION_TYPE").unwrap_or_default(),
        "app: Annex for Linux"
    );
    tracing::info!(
        session = session.describe(),
        compositor = desktop::running_compositor().as_deref().unwrap_or("none found"),
        gtk_backend = backend,
        drop_island = island,
        "app: desktop"
    );
    tracing::info!(
        gtk = format!("{}.{}.{}", gtk::major_version(), gtk::minor_version(), gtk::micro_version()),
        gstreamer = gst::version_string().as_str(),
        decoder = viewer::decoder().as_deref().unwrap_or("none"),
        encoder = encode::describe(m.settings.prefer_hardware_encoder),
        wl_clipboard = clipboard::wl_clipboard(),
        "app: libraries"
    );
    if let Some(d) = &display {
        let monitors = d.monitors();
        for i in 0..monitors.n_items() {
            let Some(mon) = monitors.item(i).and_then(|o| o.downcast::<gdk::Monitor>().ok()) else { continue };
            let g = mon.geometry();
            tracing::info!(
                index = i,
                connector = mon.connector().map(|c| c.to_string()).unwrap_or_default(),
                model = mon.model().map(|c| c.to_string()).unwrap_or_default(),
                x = g.x(),
                y = g.y(),
                width = g.width(),
                height = g.height(),
                scale = mon.scale_factor(),
                refresh_mhz = mon.refresh_rate(),
                "app: screen"
            );
        }
    }
    tracing::info!(
        data = %settings::data_dir().display(),
        config = %settings::config_dir().display(),
        name = m.device_name(),
        fingerprint = m.fingerprint_display(),
        "app: this computer"
    );
    tracing::info!(target: "annex::settings", "settings: {}", m.settings.describe());
}
