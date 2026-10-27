//! Annex for Linux: show another computer's screen, share this one, or use it as an extra
//! display. GTK 4 for the window, GStreamer for video, the shared Rust core for the rest
//! (docs/LINUX.md).

mod clipboard;
mod desktop;
mod encode;
mod extend;
mod gnome;
mod host;
mod inject;
mod log;
mod model;
mod portal;
mod screencopy;
mod settings;
mod tray;
mod ui;
mod viewer;
mod wl;

use gtk::prelude::*;
use gtk::{gio, glib};

use crate::model::Model;

const APP_ID: &str = "dev.getannex.Annex";

/// Command-line options, for open-at-login and scripts.
#[derive(Clone, Default)]
struct Opts {
    /// Start in the tray without the window (open at login).
    background: bool,
    /// Share this screen right away.
    share: bool,
    /// Nobody at this screen: turn on (and keep) the headless setting.
    headless: bool,
    /// Connect to this address (or code) right away; `--extend` to use it as a display.
    connect: Option<String>,
    extend: bool,
}

const USAGE: &str =
    "Usage: annex [--background] [--share] [--headless] [--connect <address | code> [--extend]] [--version]";

fn main() -> glib::ExitCode {
    let args: Vec<String> = std::env::args().collect();
    let mut opts = Opts::default();
    let mut it = args.iter().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--background" => opts.background = true,
            "--share" => opts.share = true,
            "--headless" => opts.headless = true,
            "--extend" => opts.extend = true,
            "--connect" => opts.connect = it.next().cloned(),
            "--version" => {
                println!("Annex {}", env!("CARGO_PKG_VERSION"));
                return glib::ExitCode::SUCCESS;
            }
            "--help" | "-h" => {
                println!("{USAGE}");
                return glib::ExitCode::SUCCESS;
            }
            other => {
                eprintln!("unknown option {other}\n{USAGE}");
                return glib::ExitCode::FAILURE;
            }
        }
    }

    if let Err(e) = gst::init() {
        eprintln!("Annex needs GStreamer 1.20 or later: {e}");
        return glib::ExitCode::FAILURE;
    }
    // The video sink is built in, so no GStreamer GTK plugin package is needed.
    if let Err(e) = gstgtk4::plugin_register_static() {
        eprintln!("couldn't register the GTK video sink: {e}");
    }

    let app = gtk::Application::builder().application_id(APP_ID).build();
    let started = std::cell::Cell::new(false);
    app.connect_activate(move |app| {
        // A second launch (or the desktop's launcher) shows the window of the running Annex.
        if started.replace(true) {
            if let Some(model) = MODEL.with(|m| m.borrow().clone()) {
                ui::present(app, &model);
            }
            return;
        }
        start(app, opts.clone());
    });
    // GTK would reject our own flags.
    app.run_with_args(&args[..1])
}

thread_local! {
    static MODEL: std::cell::RefCell<Option<model::Shared>> = const { std::cell::RefCell::new(None) };
}

fn start(app: &gtk::Application, opts: Opts) {
    ui::load_css();
    // Keep running with no window open: Annex lives in the tray.
    std::mem::forget(app.hold());

    let (tx, rx) = async_channel::unbounded();
    let model = Model::new(tx);
    MODEL.with(|m| *m.borrow_mut() = Some(model.clone()));

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
    if opts.headless && !model.borrow().settings.headless {
        let mut m = model.borrow_mut();
        m.settings.headless = true;
        m.core.set_headless(true);
        m.save_settings();
    }
    if let Some(file) = model.borrow().core.pin_file().filter(|_| model.borrow().settings.headless) {
        tracing::info!("headless: pairing PINs appear in {}", file.display());
    }
    if opts.share || model.borrow().settings.start_sharing_at_launch {
        ui::act(app, &model, |m| m.set_hosting(true));
    }
    // Without a tray there is no other way back to Annex, so the window always opens then.
    if !opts.background || !has_tray {
        ui::present(app, &model);
    }
    if let Some(to) = opts.connect {
        let display = viewer::display_info(None::<&gtk::Widget>);
        ui::act(app, &model, |m| {
            if ui::looks_like_code(&to) {
                m.connect_code(&to, opts.extend, display)
            } else {
                m.connect_address(&to, opts.extend, display)
            }
        });
    }
}

fn quit(app: &gtk::Application, model: &model::Shared) {
    model.borrow_mut().quit();
    viewer::close();
    app.quit();
}
