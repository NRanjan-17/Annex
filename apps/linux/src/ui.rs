//! The main window: Nearby, Share This Screen, Paired Devices, Settings and About, in a sidebar
//! like the Windows app. Pages keep their widgets; [`render`] refreshes only what changes, so
//! typing in a field isn't interrupted.

use std::cell::RefCell;
use std::collections::HashMap;

use annex_update::whats_new::{self, Release};
use gtk::prelude::*;
use gtk::{gio, glib};

use crate::model::{Change, InternetShare, Model, SessionRole, Shared};

struct Ui {
    window: gtk::ApplicationWindow,
    stack: gtk::Stack,
    banner: gtk::Label,
    // Home
    live: gtk::Box,
    same_name: gtk::Box,
    status: gtk::Label,
    nearby: gtk::Box,
    /// Connect by address or code: stops a connection that hasn't started yet.
    cancel_connect: gtk::Button,
    // Share
    share_switch: gtk::Switch,
    share_note: gtk::Label,
    sessions: gtk::Box,
    code: gtk::Box,
    // Paired devices
    devices: gtk::Box,
    // About
    about_details: gtk::Label,
    /// Open PIN windows, by request.
    pins: HashMap<u64, gtk::Window>,
    /// The last render's lists, so unchanged lists aren't rebuilt.
    last: HashMap<&'static str, String>,
}

thread_local! {
    static UI: RefCell<Option<Ui>> = const { RefCell::new(None) };
    /// "What's new" waiting for the window (Annex started in the tray).
    static WHATS_NEW: RefCell<Vec<Release>> = const { RefCell::new(Vec::new()) };
}

const CSS: &str = "
.page { padding: 24px; }
.title { font-size: 20px; font-weight: 700; }
.section { font-weight: 700; margin-top: 12px; }
.dim { opacity: 0.65; }
.card { border-radius: 10px; padding: 12px 14px; background: alpha(currentColor, 0.06); }
.live { border-radius: 10px; padding: 12px 14px; background: alpha(@accent_bg_color, 0.18); }
.role { font-size: 15px; font-weight: 700; }
.code { font-family: monospace; font-size: 28px; font-weight: 700; letter-spacing: 4px; }
.pin { font-family: monospace; font-size: 40px; font-weight: 700; letter-spacing: 8px; }
.banner { padding: 8px 12px; background: alpha(#e5a50a, 0.25); }
.version { font-size: 17px; font-weight: 700; margin-top: 16px; }
.video-window, .video { background: black; }
";

pub fn load_css() {
    let provider = gtk::CssProvider::new();
    provider.load_from_data(CSS);
    if let Some(display) = gtk::gdk::Display::default() {
        gtk::style_context_add_provider_for_display(&display, &provider, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);
    }
}

/// Runs `f` on the model, then acts on what changed once the borrow is over.
pub fn act(app: &gtk::Application, model: &Shared, f: impl FnOnce(&mut Model)) {
    f(&mut model.borrow_mut());
    apply(app, model);
}

/// Acts on the model's queued changes (the model never calls the UI while borrowed).
pub fn apply(app: &gtk::Application, model: &Shared) {
    loop {
        let changes = model.borrow_mut().take_changes();
        if changes.is_empty() {
            break;
        }
        for c in changes {
            match c {
                Change::Render => {
                    render(model);
                    crate::tray::refresh(&model.borrow());
                }
                Change::PinRequired { request_id, peer_name, fingerprint } => {
                    ask_pin(app, model, request_id, &peer_name, &fingerprint)
                }
                Change::ShowPin { request_id, pin, peer_name, fingerprint } => {
                    show_pin(app, model, request_id, &pin, &peer_name, &fingerprint)
                }
                Change::PinDone { request_id } => {
                    tracing::debug!(target: "annex::pairing", request_id, "pairing: done; closing its window");
                    if let Some(w) = with_ui(|ui| ui.pins.remove(&request_id)).flatten() {
                        w.destroy();
                    }
                }
                Change::InputRequested { peer_name, fingerprint } => ask_input(app, model, &peer_name, &fingerprint),
                Change::Notify { title, body } => {
                    tracing::debug!(title, "ui: notification");
                    let n = gio::Notification::new(&title);
                    n.set_body(Some(&body));
                    app.send_notification(Some("session"), &n);
                }
                Change::ViewerOpen => crate::viewer::open(app, model),
                Change::ViewerClose => crate::viewer::close(),
                Change::ViewerStream { width, height } => crate::viewer::stream_size(width, height),
            }
        }
    }
}

fn with_ui<R>(f: impl FnOnce(&mut Ui) -> R) -> Option<R> {
    UI.with(|u| u.borrow_mut().as_mut().map(f))
}

pub fn present(app: &gtk::Application, model: &Shared) {
    if with_ui(|ui| ui.window.present()).is_none() {
        build(app, model);
        with_ui(|ui| ui.window.present());
    }
    let notes = WHATS_NEW.with(|w| std::mem::take(&mut *w.borrow_mut()));
    if !notes.is_empty() {
        let (app, model) = (app.clone(), model.clone());
        // Once the window is up, so the dialog sits on it.
        glib::idle_add_local_once(move || {
            tracing::info!(target: "annex::update", "update: showing what's new for {}", notes[0].version);
            show_whats_new(&app, &notes);
            mark_seen(&model);
        });
    }
}

/// After an update, the new releases' notes show once, with the window. A first install shows
/// nothing and only remembers this version.
pub fn whats_new_at_launch(model: &Shared, used_before: bool) {
    let current = whats_new::CURRENT;
    let saved = model.borrow().settings.last_seen_version.clone();
    // 0.3.0 didn't save it: Annex already used here means an update from 0.3.0.
    let last_seen = saved.clone().or_else(|| used_before.then(|| "0.3.0".to_string()));
    let notes = whats_new::since(last_seen.as_deref(), current);
    tracing::info!(
        target: "annex::update",
        current,
        last_seen = last_seen.as_deref().unwrap_or("none (first install)"),
        releases = notes.len(),
        "update: what's new"
    );
    if !notes.is_empty() {
        WHATS_NEW.with(|w| *w.borrow_mut() = notes);
    } else if saved.as_deref() != Some(current) {
        mark_seen(model);
    }
}

fn mark_seen(model: &Shared) {
    let mut m = model.borrow_mut();
    m.settings.last_seen_version = Some(whats_new::CURRENT.to_string());
    m.save_settings();
}

/// Shows the window at a page: "home", "share", "devices", "settings" or "about".
pub fn show_pane(app: &gtk::Application, model: &Shared, page: &str) {
    present(app, model);
    with_ui(|ui| ui.stack.set_visible_child_name(page));
}

fn label(text: &str, class: &str) -> gtk::Label {
    let l = gtk::Label::new(Some(text));
    l.set_xalign(0.0);
    l.set_wrap(true);
    if !class.is_empty() {
        l.add_css_class(class);
    }
    l
}

fn vbox(spacing: i32) -> gtk::Box {
    gtk::Box::new(gtk::Orientation::Vertical, spacing)
}

fn hbox(spacing: i32) -> gtk::Box {
    gtk::Box::new(gtk::Orientation::Horizontal, spacing)
}

fn clear(b: &gtk::Box) {
    while let Some(c) = b.first_child() {
        b.remove(&c);
    }
}

fn page(content: &gtk::Box) -> gtk::ScrolledWindow {
    content.add_css_class("page");
    gtk::ScrolledWindow::builder().child(content).hscrollbar_policy(gtk::PolicyType::Never).vexpand(true).build()
}

fn build(app: &gtk::Application, model: &Shared) {
    let window = gtk::ApplicationWindow::builder()
        .application(app)
        .title("Annex")
        .default_width(760)
        .default_height(560)
        .build();
    // Closing the window keeps Annex running in the tray.
    window.set_hide_on_close(true);

    let stack = gtk::Stack::new();
    stack.set_transition_type(gtk::StackTransitionType::None);
    let sidebar = gtk::StackSidebar::new();
    sidebar.set_stack(&stack);
    sidebar.set_width_request(190);

    // ---- Home ----
    let home = vbox(10);
    home.append(&label("Nearby", "title"));
    let live = vbox(8);
    let same_name = vbox(6);
    let status = label("", "dim");
    let nearby = vbox(8);
    home.append(&live);
    home.append(&same_name);
    home.append(&status);
    home.append(&nearby);

    home.append(&label("Connect by address or code", "section"));
    let entry = gtk::Entry::builder()
        .placeholder_text("Address (192.168.1.20 or name.local) or a code like K7M-4QX")
        .secondary_icon_name("edit-clear-symbolic")
        .secondary_icon_tooltip_text("Clear")
        .hexpand(true)
        .build();
    let row = hbox(8);
    row.append(&entry);
    let show_btn = gtk::Button::with_label("Show Its Screen");
    let extend_btn = gtk::Button::with_label("Use as Display");
    let cancel_connect = gtk::Button::with_label("Cancel");
    cancel_connect.set_visible(false);
    row.append(&show_btn);
    row.append(&extend_btn);
    row.append(&cancel_connect);
    home.append(&row);
    entry.connect_icon_press(|e, _| e.set_text(""));
    {
        let (app, model) = (app.clone(), model.clone());
        // Typing again clears the last attempt's error.
        entry.connect_changed(move |_| {
            act(&app, &model, |m| {
                if m.viewer.is_none() && m.viewer_status.take().is_some() {
                    m.render();
                }
            })
        });
    }
    {
        let (app, model, entry) = (app.clone(), model.clone(), entry.clone());
        cancel_connect.connect_clicked(move |_| {
            tracing::info!(target: "annex::viewer", "ui: Cancel pressed while connecting");
            act(&app, &model, |m| m.disconnect());
            entry.set_text("");
        });
    }
    home.append(&label(
        "A code connects over the internet; the other computer gets one from Share This Screen → Share with a code.",
        "dim",
    ));
    for (btn, extend) in [(show_btn, false), (extend_btn, true)] {
        let (app, model, entry) = (app.clone(), model.clone(), entry.clone());
        btn.connect_clicked(move |b| {
            let text = entry.text().to_string();
            tracing::info!(target: "annex::viewer", extend, "ui: {} pressed", if extend { "Use as Display" } else { "Show Its Screen" });
            let display = crate::viewer::display_info(Some(b));
            act(&app, &model, |m| {
                if looks_like_code(&text) {
                    m.connect_code(&text, extend, display)
                } else {
                    m.connect_address(&text, extend, display)
                }
            });
            // Fresh for next time (a code works only once anyway).
            entry.set_text("");
        });
    }

    // ---- Share ----
    let share = vbox(10);
    share.append(&label("Share This Screen", "title"));
    let row = hbox(12);
    let share_switch = gtk::Switch::new();
    share_switch.set_valign(gtk::Align::Center);
    row.append(&label("Let computers nearby see this screen", ""));
    let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    spacer.set_hexpand(true);
    row.append(&spacer);
    row.append(&share_switch);
    share.append(&row);
    let share_note = label("", "dim");
    share.append(&share_note);
    let sessions = vbox(8);
    share.append(&sessions);
    share.append(&label("Over the internet", "section"));
    let code = vbox(8);
    share.append(&code);
    {
        let (app, model) = (app.clone(), model.clone());
        share_switch.connect_state_set(move |_, on| {
            tracing::debug!(target: "annex::host", on, "ui: Share This Screen switch");
            let (app, model) = (app.clone(), model.clone());
            // After the switch's own handler: hosting may open a portal dialog.
            glib::idle_add_local_once(move || act(&app, &model, |m| m.set_hosting(on)));
            glib::Propagation::Proceed
        });
    }

    // ---- Paired devices ----
    let devices_page = vbox(10);
    devices_page.append(&label("Paired Devices", "title"));
    devices_page.append(&label(
        "Computers that typed a PIN once. They connect without one, and only they can control this computer's mouse and keyboard, when allowed.",
        "dim",
    ));
    let devices = vbox(8);
    devices_page.append(&devices);

    // ---- Settings ----
    let settings = settings_page(app, model);

    // ---- About ----
    let about = vbox(8);
    about.append(&label("Annex", "title"));
    let row = hbox(12);
    // The release's version (the core's; this crate's own isn't stamped).
    row.append(&label(&format!("Version {}", whats_new::CURRENT), ""));
    let news = gtk::Button::with_label("What's New");
    news.set_valign(gtk::Align::Center);
    {
        let app = app.clone();
        news.connect_clicked(move |_| {
            tracing::info!(target: "annex::update", "update: What's New pressed");
            show_whats_new(&app, &whats_new::all());
        });
    }
    row.append(&news);
    about.append(&row);
    about.append(&label("Show another computer's screen, share this one, or use it as an extra display.", "dim"));
    let about_details = label("", "dim");
    about_details.set_selectable(true);
    about.append(&about_details);
    let link = gtk::LinkButton::with_label("https://github.com/NRanjan-17/Annex", "github.com/NRanjan-17/Annex");
    link.set_halign(gtk::Align::Start);
    about.append(&link);

    stack.add_titled(&page(&home), Some("home"), "Nearby");
    stack.add_titled(&page(&share), Some("share"), "Share This Screen");
    stack.add_titled(&page(&devices_page), Some("devices"), "Paired Devices");
    stack.add_titled(&page(&settings), Some("settings"), "Settings");
    stack.add_titled(&page(&about), Some("about"), "About");

    let banner = label("", "banner");
    banner.set_visible(false);
    let main = hbox(0);
    main.append(&sidebar);
    main.append(&gtk::Separator::new(gtk::Orientation::Vertical));
    let right = vbox(0);
    right.set_hexpand(true);
    right.append(&banner);
    right.append(&stack);
    main.append(&right);
    window.set_child(Some(&main));

    {
        let (model, entry) = (model.clone(), entry.clone());
        stack.connect_visible_child_name_notify(move |s| {
            if s.visible_child_name().as_deref() == Some("home") {
                entry.grab_focus();
                if let Ok(m) = model.try_borrow() {
                    m.refresh_nearby();
                }
            }
        });
    }

    UI.with(|u| {
        *u.borrow_mut() = Some(Ui {
            window,
            stack,
            banner,
            live,
            same_name,
            status,
            nearby,
            cancel_connect,
            share_switch,
            share_note,
            sessions,
            code,
            devices,
            about_details,
            pins: HashMap::new(),
            last: HashMap::new(),
        })
    });
    render(model);
}

pub fn looks_like_code(s: &str) -> bool {
    let t: String = s.trim().chars().filter(|c| *c != '-').collect();
    t.len() == 6 && t.chars().all(|c| c.is_ascii_alphanumeric()) && !t.chars().all(|c| c.is_ascii_digit())
}

type Toggle = Box<dyn Fn(&mut Model, bool)>;
type SetText = fn(&mut Model, String);

fn settings_page(app: &gtk::Application, model: &Shared) -> gtk::Box {
    let b = vbox(10);
    b.append(&label("Settings", "title"));
    let s = model.borrow().settings.clone();
    let name = model.borrow().device_name();

    b.append(&label("This computer's name", "section"));
    let row = hbox(8);
    let name_entry = gtk::Entry::builder().text(name).hexpand(true).build();
    let save = gtk::Button::with_label("Rename");
    row.append(&name_entry);
    row.append(&save);
    b.append(&row);
    {
        let (app, model, e) = (app.clone(), model.clone(), name_entry.clone());
        let rename = move || {
            let text = e.text().to_string();
            act(&app, &model, |m| m.rename(&text));
        };
        let r = rename.clone();
        save.connect_clicked(move |_| r());
        name_entry.connect_activate(move |_| rename());
    }

    let toggle = |b: &gtk::Box, text: &str, on: bool, f: Toggle| {
        let row = hbox(12);
        row.append(&label(text, ""));
        let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        spacer.set_hexpand(true);
        row.append(&spacer);
        let sw = gtk::Switch::new();
        sw.set_active(on);
        sw.set_valign(gtk::Align::Center);
        let (app, model, name) = (app.clone(), model.clone(), text.to_string());
        sw.connect_state_set(move |_, on| {
            tracing::info!(target: "annex::settings", setting = name, on, "settings: switched");
            act(&app, &model, |m| {
                f(m, on);
                m.save_settings();
            });
            glib::Propagation::Proceed
        });
        row.append(&sw);
        b.append(&row);
    };

    b.append(&label("General", "section"));
    toggle(
        &b,
        "Open Annex at login",
        s.open_at_login,
        Box::new(|m, on| {
            m.settings.open_at_login = on;
            if let Err(e) = crate::settings::set_open_at_login(on) {
                tracing::warn!("open at login: {e}");
            }
        }),
    );
    toggle(
        &b,
        "Share this screen when Annex opens",
        s.start_sharing_at_launch,
        Box::new(|m, on| m.settings.start_sharing_at_launch = on),
    );
    toggle(
        &b,
        "No one at this screen",
        s.headless,
        Box::new(|m, on| {
            m.settings.headless = on;
            m.core.set_headless(on);
        }),
    );
    let note = label(
        "For a computer reached over SSH: pairing PINs also go to pairing-pin.txt in Annex's data folder, and paired devices can control it without asking.",
        "dim",
    );
    note.set_wrap(true);
    b.append(&note);

    b.append(&label("Viewing", "section"));
    toggle(
        &b,
        "Use this computer's mouse and keyboard",
        s.use_mouse_and_keyboard,
        Box::new(|m, on| m.settings.use_mouse_and_keyboard = on),
    );
    toggle(&b, "Open in full screen", s.open_full_screen, Box::new(|m, on| m.settings.open_full_screen = on));
    toggle(
        &b,
        "Share the clipboard",
        s.share_clipboard,
        Box::new(|m, on| {
            m.settings.share_clipboard = on;
            m.set_clipboard_sharing(on);
        }),
    );
    toggle(
        &b,
        "Drop files at the top of the screen",
        s.drop_at_top,
        Box::new(|m, on| {
            m.settings.drop_at_top = on;
            m.update_drop_target();
        }),
    );
    toggle(
        &b,
        "Receive files into Downloads",
        s.receive_files,
        Box::new(|m, on| {
            m.settings.receive_files = on;
            m.set_receive_files(on);
        }),
    );
    let row = hbox(12);
    row.append(&label("Picture", ""));
    let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    spacer.set_hexpand(true);
    row.append(&spacer);
    let quality = gtk::DropDown::from_strings(&["Best", "Balanced", "Fast"]);
    quality.set_selected(s.quality.min(2));
    {
        let (app, model) = (app.clone(), model.clone());
        quality.connect_selected_notify(move |d| {
            let q = d.selected();
            tracing::info!(target: "annex::settings", quality = q, "settings: Picture");
            act(&app, &model, |m| {
                m.settings.quality = q;
                m.save_settings();
            });
        });
    }
    row.append(&quality);
    b.append(&row);

    b.append(&label("Sharing", "section"));
    toggle(
        &b,
        "Use the graphics chip's video encoder",
        s.prefer_hardware_encoder,
        Box::new(|m, on| m.settings.prefer_hardware_encoder = on),
    );
    let row = hbox(12);
    row.append(&label("Extended display scaling", ""));
    let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    spacer.set_hexpand(true);
    row.append(&spacer);
    const SCALES: [u32; 6] = [0, 100, 125, 150, 175, 200];
    let scale = gtk::DropDown::from_strings(&["Match the other screen", "100 %", "125 %", "150 %", "175 %", "200 %"]);
    scale.set_selected(SCALES.iter().position(|&p| p == s.extend_scale).unwrap_or(0) as u32);
    {
        let (app, model) = (app.clone(), model.clone());
        scale.connect_selected_notify(move |d| {
            let p = SCALES[(d.selected() as usize).min(SCALES.len() - 1)];
            act(&app, &model, |m| {
                m.settings.extend_scale = p;
                m.save_settings();
            });
        });
    }
    row.append(&scale);
    b.append(&row);
    b.append(&label(
        "Text size on a computer using this one as an extra display. Turn sharing off and on to apply.",
        "dim",
    ));

    b.append(&label("Sound", "section"));
    let sound: [(&str, &str, bool, fn(&mut crate::settings::Settings, bool)); 3] = [
        ("Share this computer's sound", "People viewing this computer hear what it plays.", s.share_sound, |s, on| {
            s.share_sound = on
        }),
        (
            "Hear the other computer",
            "Plays the shared computer's sound here, and a viewer's microphone.",
            s.hear_sound,
            |s, on| s.hear_sound = on,
        ),
        (
            "Send my microphone",
            "Your voice plays on the computer you're viewing. Use headphones to avoid an echo.",
            s.send_mic,
            |s, on| s.send_mic = on,
        ),
    ];
    let virtual_mic = annex_session::audio::virtual_microphone_here().then_some((
        "Use a viewer's microphone as Annex Microphone",
        "Apps here, like Zoom or Meet, can pick Annex Microphone. Off: it plays aloud.",
        s.virtual_mic,
        (|s, on| s.virtual_mic = on) as fn(&mut crate::settings::Settings, bool),
    ));
    for (text, caption, on, set) in sound.into_iter().chain(virtual_mic) {
        toggle(
            &b,
            text,
            on,
            Box::new(move |m, on| {
                set(&mut m.settings, on);
                m.core.set_audio(m.settings.audio());
            }),
        );
        let note = label(caption, "dim");
        note.set_wrap(true);
        b.append(&note);
    }

    b.append(&label("Internet", "section"));
    b.append(&label("Codes and relays go through Annex's server unless you use your own.", "dim"));
    let fields: [(&str, String, SetText); 4] = [
        ("Code server (wss://…)", s.rendezvous_server.clone(), |m, v| m.settings.rendezvous_server = v),
        ("Your relay (turn:host:3478)", s.relay_url.clone(), |m, v| m.settings.relay_url = v),
        ("Relay user name", s.relay_username.clone(), |m, v| m.settings.relay_username = v),
        ("Relay password", s.relay_password.clone(), |m, v| m.settings.relay_password = v),
    ];
    for (i, (hint, value, set)) in fields.into_iter().enumerate() {
        let e = gtk::Entry::builder().placeholder_text(hint).text(value).build();
        if i == 3 {
            e.set_visibility(false);
        }
        let (app, model) = (app.clone(), model.clone());
        e.connect_changed(move |e| {
            let v = e.text().to_string();
            // The field's name only: never what's typed (a relay password, for one).
            tracing::debug!(target: "annex::settings", field = hint, empty = v.is_empty(), "settings: edited");
            act(&app, &model, |m| {
                set(m, v);
                m.save_settings();
            });
        });
        b.append(&e);
    }

    b.append(&label("Troubleshooting", "section"));
    toggle(
        &b,
        "Show technical details",
        s.show_technical_details,
        Box::new(|m, on| m.settings.show_technical_details = on),
    );
    toggle(
        &b,
        "Verbose logging",
        s.verbose_logging,
        Box::new(|m, on| {
            m.settings.verbose_logging = on;
            annex_log::set_verbose(on);
        }),
    );
    let log_dir = annex_log::dir().unwrap_or_else(crate::settings::data_dir);
    b.append(&label(
        &format!(
            "Annex always keeps a log at {}, and a copy of what led up to each error in its crashes \
             folder. It stays on this computer; Annex never sends it anywhere. If something goes \
             wrong, attach it to a bug report. Verbose logging adds every mouse and key event \
             (not which key) and fills the log faster.",
            log_dir.join("annex.log").display()
        ),
        "dim",
    ));
    let show = gtk::Button::with_label("Show Log Files");
    show.set_halign(gtk::Align::Start);
    show.connect_clicked(move |_| {
        tracing::info!(target: "annex::settings", "settings: Show Log Files");
        let uri = gio::File::for_path(&log_dir).uri();
        if let Err(e) = gio::AppInfo::launch_default_for_uri(&uri, None::<&gio::AppLaunchContext>) {
            tracing::warn!(target: "annex::settings", "settings: couldn't open the log folder: {e}");
        }
    });
    b.append(&show);
    b
}

/// Refreshes the window from the model.
pub fn render(model: &Shared) {
    let Ok(m) = model.try_borrow() else { return };
    with_ui(|ui| {
        let banner = m.startup_error.clone().or_else(|| m.host_error.clone());
        ui.banner.set_visible(banner.is_some());
        ui.banner.set_text(banner.as_deref().unwrap_or(""));

        render_live(ui, &m, model);
        render_nearby(ui, &m, model);
        render_share(ui, &m, model);
        render_devices(ui, &m, model);

        let details = format!(
            "This computer: {}\nIdentity: {}\nDesktop: {}\nVideo decoder: {}\nVideo encoder: {}",
            m.device_name(),
            m.fingerprint_display(),
            crate::desktop::session().describe(),
            crate::viewer::decoder().unwrap_or_else(|| "none found".into()),
            crate::encode::describe(m.settings.prefer_hardware_encoder),
        );
        ui.about_details.set_text(&details);
    });
}

/// Rebuilds a list only when its content changed (`key` is a summary of what it shows).
fn changed(ui: &mut Ui, list: &'static str, key: String) -> bool {
    if ui.last.get(list) == Some(&key) {
        return false;
    }
    ui.last.insert(list, key);
    true
}

fn app_of(ui: &Ui) -> gtk::Application {
    ui.window.application().expect("window has an application")
}

fn render_live(ui: &mut Ui, m: &Model, model: &Shared) {
    let role = m.role();
    let latency = if m.settings.show_technical_details {
        m.latency.as_ref().map(|l| format!(" · {:.0} fps · {:.0} ms", l.fps, l.p50_ms)).unwrap_or_default()
    } else {
        String::new()
    };
    let key = format!("{role:?}{latency}{}", m.viewer_route);
    if changed(ui, "live", key) {
        clear(&ui.live);
        if let Some(role) = &role {
            let card = vbox(4);
            card.add_css_class("live");
            card.append(&label(&role.headline(), "role"));
            let (flow, stop) = match role {
                SessionRole::Sharing { with, extend } => (
                    if *extend {
                        format!("This computer → {} (its extra display)", m.label(with))
                    } else {
                        format!("This computer → {}", m.label(with))
                    },
                    "Stop Sharing",
                ),
                SessionRole::Viewing { from, extend } => (
                    if *extend {
                        format!("{} → this computer (an extra display)", m.label(from))
                    } else {
                        format!("{} → this computer", m.label(from))
                    },
                    "Disconnect",
                ),
            };
            card.append(&label(&format!("{flow}{latency}"), ""));
            if !m.viewer_route.is_empty() && matches!(role, SessionRole::Viewing { .. }) {
                card.append(&label(&m.viewer_route, "dim"));
            }
            let b = gtk::Button::with_label(stop);
            b.set_halign(gtk::Align::Start);
            let (app, model, viewing) = (app_of(ui), model.clone(), matches!(role, SessionRole::Viewing { .. }));
            b.connect_clicked(move |_| {
                tracing::info!(target: "annex::session", "ui: {} pressed", if viewing { "Disconnect" } else { "Stop Sharing" });
                act(&app, &model, |m| if viewing { m.disconnect() } else { m.set_hosting(false) });
            });
            card.append(&b);
            ui.live.append(&card);
        }
    }

    let same = m.same_name_peer();
    if changed(ui, "same", format!("{same:?}")) {
        clear(&ui.same_name);
        if let Some(name) = same {
            let card = vbox(4);
            card.add_css_class("card");
            card.append(&label(
                &format!(
                    "Another computer is also called “{name}”. Rename this one in Settings so you can tell them apart."
                ),
                "",
            ));
            ui.same_name.append(&card);
        }
    }

    ui.status.set_text(m.viewer_status.as_deref().unwrap_or(""));
    ui.status.set_visible(m.viewer_status.is_some() && role.is_none());
    ui.cancel_connect.set_visible(m.viewer.is_some() && m.viewer_extends.is_none());
}

fn render_nearby(ui: &mut Ui, m: &Model, model: &Shared) {
    let peers = m.peers();
    let key = format!("{peers:?}{}{}", m.trusted.len(), m.viewer.is_some());
    if !changed(ui, "nearby", key) {
        return;
    }
    clear(&ui.nearby);
    if peers.is_empty() {
        ui.nearby.append(&label(
            "Looking for computers with Annex open… They show up here when they're on the same network or tailnet.",
            "dim",
        ));
        return;
    }
    for p in peers {
        let card = hbox(10);
        card.add_css_class("card");
        let text = vbox(2);
        text.set_hexpand(true);
        let name = m.label(&p.name);
        text.append(&label(&name, "role"));
        let mut sub = Vec::new();
        if m.has_this_name(&p.name) {
            sub.push("Other computer".to_string());
        }
        sub.push(if p.sharing { "Sharing its screen".into() } else { "Annex is open, not sharing".into() });
        if m.is_paired(&p) {
            sub.push("Paired".into());
        }
        if p.mesh {
            sub.push("Private network".into());
        }
        text.append(&label(&sub.join(" · "), "dim"));
        card.append(&text);
        if p.sharing {
            let show = gtk::Button::with_label("Show Its Screen");
            show.set_valign(gtk::Align::Center);
            card.append(&show);
            let mut buttons = vec![(show, false)];
            if p.extend == Some(true) {
                let ext = gtk::Button::with_label("Use as Display");
                ext.set_valign(gtk::Align::Center);
                card.append(&ext);
                buttons.push((ext, true));
            }
            for (b, extend) in buttons {
                let (app, model, peer) = (app_of(ui), model.clone(), p.clone());
                b.connect_clicked(move |b| {
                    tracing::info!(target: "annex::viewer", peer = peer.name, extend, "ui: connect pressed in Nearby");
                    let display = crate::viewer::display_info(Some(b));
                    act(&app, &model, |m| m.connect_peer(&peer, extend, display));
                });
            }
        }
        ui.nearby.append(&card);
    }
}

fn render_share(ui: &mut Ui, m: &Model, model: &Shared) {
    if ui.share_switch.is_active() != m.is_hosting() {
        ui.share_switch.set_active(m.is_hosting());
    }
    ui.share_note.set_text(&if m.is_hosting() {
        format!(
            "Computers nearby see “{}”. The first time, they type a PIN shown here. {}",
            m.device_name(),
            crate::host::extend_note()
        )
    } else {
        "Off. Nothing on this computer is shown to anyone.".into()
    });

    let key = format!("{:?}", m.host_sessions);
    if changed(ui, "sessions", key) {
        clear(&ui.sessions);
        for s in &m.host_sessions {
            let card = hbox(10);
            card.add_css_class("card");
            let text = vbox(2);
            text.set_hexpand(true);
            text.append(&label(&m.label(&s.name), "role"));
            text.append(&label(
                &format!(
                    "{} · {}",
                    if s.extend { "Using this computer as a display" } else { "Viewing this screen" },
                    if s.input { "Can use the mouse and keyboard" } else { "Can only watch" }
                ),
                "dim",
            ));
            card.append(&text);
            ui.sessions.append(&card);
        }
    }

    let key = format!("{:?}{}", m.internet, m.is_hosting());
    if changed(ui, "code", key) {
        clear(&ui.code);
        if !m.is_hosting() {
            ui.code.append(&label("Turn on sharing to get a code.", "dim"));
            return;
        }
        let app = app_of(ui);
        let button = |text: &str, f: fn(&mut Model)| {
            let b = gtk::Button::with_label(text);
            b.set_halign(gtk::Align::Start);
            let (app, model) = (app.clone(), model.clone());
            b.connect_clicked(move |_| act(&app, &model, f));
            b
        };
        match &m.internet {
            None => {
                ui.code
                    .append(&label("A code lets one computer anywhere connect. It works once, for 10 minutes.", "dim"));
                ui.code.append(&button("Share with a Code", |m| m.share_code()));
            }
            Some(InternetShare::Starting) => ui.code.append(&label("Getting a code…", "dim")),
            Some(InternetShare::Code { code, .. }) => {
                let c = label(code, "code");
                c.set_selectable(true);
                ui.code.append(&c);
                ui.code.append(&label("Type it on the other computer, in Nearby → Connect by address or code.", "dim"));
                ui.code.append(&button("Stop", |m| m.stop_code()));
            }
            Some(InternetShare::Joining(name)) => {
                ui.code.append(&label(&format!("{} is connecting…", m.label(name)), ""))
            }
            Some(InternetShare::Relay { name, relayed, limited }) => {
                let text = match (relayed, limited) {
                    (true, true) => {
                        format!("{} is connecting through Annex's relay (limited time and quality).", m.label(name))
                    }
                    (true, false) => format!("{} is connecting through your relay.", m.label(name)),
                    (false, _) => format!("{} couldn't connect directly.", m.label(name)),
                };
                ui.code.append(&label(&text, ""));
            }
            Some(InternetShare::Ended(why)) => {
                if let Some(why) = why {
                    ui.code.append(&label(why, ""));
                }
                ui.code.append(&button("Share with a New Code", |m| m.share_code()));
            }
        }
    }
}

fn render_devices(ui: &mut Ui, m: &Model, model: &Shared) {
    let key = format!(
        "{:?} {}",
        m.trusted.iter().map(|t| (&t.fingerprint, &t.name, t.allow_input)).collect::<Vec<_>>(),
        m.settings.headless
    );
    if !changed(ui, "devices", key) {
        return;
    }
    clear(&ui.devices);
    if m.trusted.is_empty() {
        ui.devices.append(&label("None yet.", "dim"));
        return;
    }
    for t in &m.trusted {
        let card = hbox(10);
        card.add_css_class("card");
        let text = vbox(2);
        text.set_hexpand(true);
        text.append(&label(&m.label(&t.name), "role"));
        text.append(&label(&crate::model::display_fingerprint(&t.fingerprint), "dim"));
        card.append(&text);
        let input = gtk::CheckButton::with_label("Mouse and keyboard");
        // Headless lets every paired device control this computer.
        input.set_active(t.allow_input || m.settings.headless);
        input.set_sensitive(!m.settings.headless);
        if m.settings.headless {
            input.set_tooltip_text(Some("Allowed while No one at this screen is on"));
        }
        input.set_valign(gtk::Align::Center);
        let (app, model2, fp) = (app_of(ui), model.clone(), t.fingerprint.clone());
        input.connect_toggled(move |c| {
            let on = c.is_active();
            let fp = fp.clone();
            act(&app, &model2, |m| m.set_allow_input(&fp, on));
        });
        card.append(&input);
        let forget = gtk::Button::with_label("Forget");
        forget.set_valign(gtk::Align::Center);
        let (app, model2, fp) = (app_of(ui), model.clone(), t.fingerprint.clone());
        forget.connect_clicked(move |_| {
            let fp = fp.clone();
            act(&app, &model2, |m| m.forget(&fp));
        });
        card.append(&forget);
        ui.devices.append(&card);
    }
}

// ---- Dialogs -------------------------------------------------------------------------------

fn dialog(app: &gtk::Application, title: &str) -> (gtk::Window, gtk::Box) {
    let w = gtk::Window::builder().application(app).title(title).modal(true).resizable(false).build();
    if let Some(parent) = with_ui(|ui| ui.window.clone()).filter(|p| p.is_visible()) {
        w.set_transient_for(Some(&parent));
    }
    let b = vbox(12);
    b.add_css_class("page");
    b.set_width_request(380);
    w.set_child(Some(&b));
    (w, b)
}

/// The release notes: each release newest first, its items under New, Improved and Fixed.
pub fn show_whats_new(app: &gtk::Application, notes: &[Release]) {
    let Some(newest) = notes.first() else { return };
    let (w, b) = dialog(app, "What's New");
    b.set_width_request(500);
    b.append(&label(&format!("What's new in Annex {}", newest.version), "title"));
    let body = vbox(6);
    let text = |t: &str, class: &str| {
        let l = label(t, class);
        l.set_max_width_chars(60);
        l
    };
    for r in notes {
        if notes.len() > 1 {
            body.append(&text(&format!("Annex {}", r.version), "version"));
        }
        for (kind, heading) in [("new", "New"), ("improved", "Improved"), ("fixed", "Fixed")] {
            let items: Vec<_> = r.items.iter().filter(|i| i.kind == kind).collect();
            if items.is_empty() {
                continue;
            }
            body.append(&text(heading, "section"));
            for i in items {
                let item = vbox(2);
                item.append(&text(&i.title, "role"));
                item.append(&text(&i.detail, ""));
                body.append(&item);
            }
        }
    }
    let scroll = gtk::ScrolledWindow::builder()
        .child(&body)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .propagate_natural_height(true)
        .max_content_height(420)
        .build();
    b.append(&scroll);
    let ok = gtk::Button::with_label("Got it");
    ok.add_css_class("suggested-action");
    ok.set_halign(gtk::Align::End);
    b.append(&ok);
    let w2 = w.clone();
    ok.connect_clicked(move |_| w2.close());
    let keys = gtk::EventControllerKey::new();
    let w2 = w.clone();
    keys.connect_key_pressed(move |_, key, _, _| {
        if key == gtk::gdk::Key::Escape {
            w2.close();
            glib::Propagation::Stop
        } else {
            glib::Propagation::Proceed
        }
    });
    w.add_controller(keys);
    w.connect_close_request(|_| {
        tracing::info!(target: "annex::update", "update: what's new seen");
        glib::Propagation::Proceed
    });
    w.present();
    ok.grab_focus();
}

fn ask_pin(app: &gtk::Application, model: &Shared, request_id: u64, peer: &str, fingerprint: &str) {
    let (w, b) = dialog(app, "Pair with Annex");
    b.append(&label(&format!("Type the PIN shown on {peer}."), "role"));
    b.append(&label(&format!("Its identity: {fingerprint}"), "dim"));
    let entry = gtk::Entry::builder()
        .placeholder_text("6-digit PIN")
        .max_length(6)
        .input_purpose(gtk::InputPurpose::Digits)
        .build();
    b.append(&entry);
    let row = hbox(8);
    row.set_halign(gtk::Align::End);
    let cancel = gtk::Button::with_label("Cancel");
    let ok = gtk::Button::with_label("Pair");
    ok.add_css_class("suggested-action");
    row.append(&cancel);
    row.append(&ok);
    b.append(&row);
    tracing::info!(target: "annex::pairing", request_id, peer, "pairing: asking for the PIN shown on the other computer");
    let submit = {
        let (model, entry, w) = (model.clone(), entry.clone(), w.clone());
        move || {
            model.borrow().submit_pin(request_id, entry.text().trim());
            w.set_sensitive(false);
        }
    };
    let s = submit.clone();
    ok.connect_clicked(move |_| s());
    entry.connect_activate(move |_| submit());
    {
        let (model, w2) = (model.clone(), w.clone());
        cancel.connect_clicked(move |_| {
            model.borrow().cancel_request(request_id);
            w2.destroy();
        });
    }
    with_ui(|ui| ui.pins.insert(request_id, w.clone()));
    w.present();
}

fn show_pin(app: &gtk::Application, model: &Shared, request_id: u64, pin: &str, peer: &str, fingerprint: &str) {
    // Never the PIN itself.
    tracing::info!(target: "annex::pairing", request_id, peer, "pairing: showing a PIN");
    let (w, b) = dialog(app, "Pair with Annex");
    b.append(&label(&format!("{peer} wants to connect. Type this PIN there:"), "role"));
    let p = label(pin, "pin");
    p.set_xalign(0.5);
    b.append(&p);
    b.append(&label(&format!("Its identity: {fingerprint}. Only type the PIN on a computer you know."), "dim"));
    let cancel = gtk::Button::with_label("Don't Allow");
    cancel.set_halign(gtk::Align::End);
    {
        let (model, w2) = (model.clone(), w.clone());
        cancel.connect_clicked(move |_| {
            model.borrow().cancel_request(request_id);
            w2.destroy();
        });
    }
    b.append(&cancel);
    with_ui(|ui| ui.pins.insert(request_id, w.clone()));
    w.present();
}

fn ask_input(app: &gtk::Application, model: &Shared, peer: &str, fingerprint: &str) {
    tracing::info!(target: "annex::permissions", peer, fingerprint, "sharing: asking to allow control");
    let (w, b) = dialog(app, "Allow control?");
    b.append(&label(&format!("{peer} wants to use this computer's mouse and keyboard."), "role"));
    b.append(&label("You can change this later in Paired Devices.", "dim"));
    let row = hbox(8);
    row.set_halign(gtk::Align::End);
    let no = gtk::Button::with_label("Don't Allow");
    let yes = gtk::Button::with_label("Allow");
    yes.add_css_class("suggested-action");
    row.append(&no);
    row.append(&yes);
    b.append(&row);
    let w2 = w.clone();
    no.connect_clicked(move |_| {
        tracing::info!(target: "annex::permissions", "sharing: control not allowed");
        w2.destroy()
    });
    let (app, model, fp, w2) = (app.clone(), model.clone(), fingerprint.to_string(), w.clone());
    yes.connect_clicked(move |_| {
        tracing::info!(target: "annex::permissions", "sharing: control allowed");
        let fp = fp.clone();
        act(&app, &model, |m| m.set_allow_input(&fp, true));
        w2.destroy();
    });
    w.present();
}
