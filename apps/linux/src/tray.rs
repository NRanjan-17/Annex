//! The tray icon and its menu (StatusNotifierItem: KDE, labwc/wf-panel on Raspberry Pi OS,
//! Cinnamon, XFCE; GNOME with the AppIndicator extension, which Ubuntu ships on).
//!
//! The menu runs on ksni's thread from a snapshot of the model; clicks come back to the main
//! thread as [`Action`]s.

use std::sync::OnceLock;

use annex_discovery::PeerInfo;
use ksni::blocking::{Handle, TrayMethods};
use ksni::menu::{CheckmarkItem, StandardItem, SubMenu};
use ksni::{MenuItem, ToolTip};

use crate::model::{Model, SessionRole};

pub enum Action {
    Show,
    Settings,
    Share(bool),
    Connect { peer: PeerInfo, extend: bool },
    Disconnect,
    Quit,
}

impl Action {
    /// For the log.
    pub fn describe(&self) -> String {
        match self {
            Action::Show => "Open Annex".into(),
            Action::Settings => "Settings".into(),
            Action::Share(on) => format!("Share This Screen {}", if *on { "on" } else { "off" }),
            Action::Connect { peer, extend } => {
                format!("{} on {}", if *extend { "Use as Display" } else { "Show Its Screen" }, peer.name)
            }
            Action::Disconnect => "Disconnect".into(),
            Action::Quit => "Quit Annex".into(),
        }
    }
}

#[derive(Clone, Default)]
struct Snapshot {
    name: String,
    hosting: bool,
    role: Option<(SessionRole, String)>,
    viewing: bool,
    /// (label, peer) for computers that share their screen.
    nearby: Vec<(String, PeerInfo)>,
}

struct Tray {
    s: Snapshot,
    tx: async_channel::Sender<Action>,
}

impl Tray {
    fn send(&self, a: Action) {
        let _ = self.tx.send_blocking(a);
    }
}

impl ksni::Tray for Tray {
    fn id(&self) -> String {
        "annex".into()
    }

    fn title(&self) -> String {
        "Annex".into()
    }

    fn icon_name(&self) -> String {
        // Installed by the packages (hicolor theme).
        "annex".into()
    }

    /// A badge from the icon theme for what this computer is doing (KDE and others show it).
    fn overlay_icon_name(&self) -> String {
        match (&self.s.role, self.s.hosting) {
            (Some((SessionRole::Viewing { .. }, _)), _) => "media-playback-start",
            (Some(_), _) | (None, true) => "network-transmit",
            _ => "",
        }
        .into()
    }

    fn tool_tip(&self) -> ToolTip {
        ToolTip {
            title: "Annex".into(),
            description: match &self.s.role {
                Some((r, _)) => r.headline(),
                None if self.s.hosting => "Sharing this screen".into(),
                None => "Not sharing".into(),
            },
            ..Default::default()
        }
    }

    fn activate(&mut self, _x: i32, _y: i32) {
        self.send(Action::Show);
    }

    fn menu(&self) -> Vec<MenuItem<Self>> {
        let mut items: Vec<MenuItem<Self>> = Vec::new();
        let disabled = |label: String| StandardItem { label, enabled: false, ..Default::default() }.into();

        if let Some((role, flow)) = &self.s.role {
            items.push(disabled(role.headline()));
            items.push(disabled(flow.clone()));
            let viewing = self.s.viewing;
            items.push(
                StandardItem {
                    label: if viewing { "Disconnect".into() } else { "Stop Sharing".into() },
                    activate: Box::new(move |t: &mut Self| {
                        t.send(if viewing { Action::Disconnect } else { Action::Share(false) })
                    }),
                    ..Default::default()
                }
                .into(),
            );
            items.push(MenuItem::Separator);
        } else {
            items.push(disabled(format!("This computer: {}", self.s.name)));
        }

        items.push(
            CheckmarkItem {
                label: "Share This Screen".into(),
                checked: self.s.hosting,
                activate: Box::new(|t: &mut Self| {
                    let on = !t.s.hosting;
                    t.send(Action::Share(on))
                }),
                ..Default::default()
            }
            .into(),
        );

        // Nearby: hidden during a session (it keeps looking in the background).
        if self.s.role.is_none() {
            items.push(MenuItem::Separator);
            if self.s.nearby.is_empty() {
                items.push(disabled("No computers nearby are sharing".into()));
            } else {
                items.push(disabled("Nearby".into()));
                for (label, peer) in &self.s.nearby {
                    let mut sub: Vec<MenuItem<Self>> = Vec::new();
                    let p = peer.clone();
                    sub.push(
                        StandardItem {
                            label: "Show Its Screen".into(),
                            activate: Box::new(move |t: &mut Self| {
                                t.send(Action::Connect { peer: p.clone(), extend: false })
                            }),
                            ..Default::default()
                        }
                        .into(),
                    );
                    if peer.extend == Some(true) {
                        let p = peer.clone();
                        sub.push(
                            StandardItem {
                                label: "Use as Display".into(),
                                activate: Box::new(move |t: &mut Self| {
                                    t.send(Action::Connect { peer: p.clone(), extend: true })
                                }),
                                ..Default::default()
                            }
                            .into(),
                        );
                    }
                    items.push(SubMenu { label: label.clone(), submenu: sub, ..Default::default() }.into());
                }
            }
        }

        items.push(MenuItem::Separator);
        items.push(
            StandardItem {
                label: "Open Annex".into(),
                activate: Box::new(|t: &mut Self| t.send(Action::Show)),
                ..Default::default()
            }
            .into(),
        );
        items.push(
            StandardItem {
                label: "Settings…".into(),
                activate: Box::new(|t: &mut Self| t.send(Action::Settings)),
                ..Default::default()
            }
            .into(),
        );
        items.push(
            StandardItem {
                label: "Quit Annex".into(),
                activate: Box::new(|t: &mut Self| t.send(Action::Quit)),
                ..Default::default()
            }
            .into(),
        );
        items
    }
}

static HANDLE: OnceLock<Handle<Tray>> = OnceLock::new();

/// Starts the tray icon. Without a tray host (plain GNOME), Annex still works from its window.
pub fn start(model: &Model, tx: async_channel::Sender<Action>) -> bool {
    let tray = Tray { s: snapshot(model), tx };
    match tray.spawn() {
        Ok(h) => {
            tracing::info!("tray: icon shown");
            let _ = HANDLE.set(h);
            true
        }
        Err(e) => {
            tracing::info!("no tray icon: {e}");
            false
        }
    }
}

pub fn refresh(model: &Model) {
    if let Some(h) = HANDLE.get() {
        let s = snapshot(model);
        h.update(move |t| t.s = s);
    }
}

fn snapshot(m: &Model) -> Snapshot {
    let role = m.role().map(|r| {
        let flow = match &r {
            SessionRole::Sharing { with, .. } => format!("This computer → {}", m.label(with)),
            SessionRole::Viewing { from, .. } => format!("{} → this computer", m.label(from)),
        };
        (r, flow)
    });
    Snapshot {
        name: m.device_name(),
        hosting: m.is_hosting(),
        viewing: m.viewer.is_some(),
        role,
        nearby: m.peers().into_iter().filter(|p| p.sharing).map(|p| (m.label(&p.name), p)).collect(),
    }
}
