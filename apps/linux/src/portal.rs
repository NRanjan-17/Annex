//! Screen sharing through xdg-desktop-portal (GNOME, KDE, and other Wayland desktops): the
//! ScreenCast portal for the picture, RemoteDesktop for the mouse and keyboard where offered.
//!
//! The desktop asks the user once; the restore token in the settings skips that dialog next
//! time on desktops that keep permissions (GNOME 44+, KDE 6).

use std::os::fd::OwnedFd;

use ashpd::desktop::PersistMode;
use ashpd::desktop::remote_desktop::{Axis, DeviceType, KeyState, RemoteDesktop};
use ashpd::desktop::screencast::{CursorMode, Screencast, SourceType};
use tokio::sync::mpsc;

pub enum PortalInput {
    Motion { x: f64, y: f64 },
    Delta { dx: f64, dy: f64 },
    Button { code: i32, down: bool },
    Scroll { dx: f64, dy: f64 },
    Key { code: i32, down: bool },
}

/// A started screen-sharing session. Dropping it ends the session.
pub struct Started {
    pub fd: OwnedFd,
    pub node: u32,
    /// The stream's size in logical pixels, when the portal says.
    pub size: Option<(i32, i32)>,
    pub token: Option<String>,
    /// Some when the portal gave mouse and keyboard access.
    pub input: Option<mpsc::UnboundedSender<PortalInput>>,
    _stop: tokio::sync::oneshot::Sender<()>,
}

fn err(e: ashpd::Error) -> String {
    match e {
        ashpd::Error::Response(ashpd::desktop::ResponseError::Cancelled) => {
            tracing::info!(target: "annex::permissions", "portal: the user didn't allow screen sharing");
            "Screen sharing wasn't allowed. Turn sharing on again and choose a screen.".into()
        }
        e => {
            tracing::warn!(target: "annex::permissions", "portal: {e:?}");
            format!("The desktop's screen-sharing service failed: {e}")
        }
    }
}

/// Asks the desktop for a screen (and the mouse and keyboard). `virtual_display`: a new,
/// empty display instead of an existing one (KDE; Extend).
pub async fn start(token: Option<String>, virtual_display: bool) -> Result<Started, String> {
    let source = if virtual_display { SourceType::Virtual } else { SourceType::Monitor };
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let screencast = Screencast::new().await.map_err(err)?;

    // RemoteDesktop with a screen: one dialog for both.
    match RemoteDesktop::new().await {
        Ok(rd) if rd.available_device_types().await.is_ok_and(|t| t.contains(DeviceType::Pointer)) => {
            tracing::info!(
                virtual_display,
                saved = token.is_some(),
                "portal: RemoteDesktop with a screen (one dialog)"
            );
            let session = rd.create_session().await.map_err(err)?;
            rd.select_devices(
                &session,
                DeviceType::Keyboard | DeviceType::Pointer,
                token.as_deref(),
                PersistMode::ExplicitlyRevoked,
            )
            .await
            .map_err(err)?;
            screencast
                .select_sources(&session, CursorMode::Embedded, source.into(), false, None, PersistMode::DoNot)
                .await
                .map_err(err)?;
            let selected = rd.start(&session, None).await.map_err(err)?.response().map_err(err)?;
            let stream = selected.streams().and_then(|s| s.first()).ok_or("The desktop shared no screen.")?.clone();
            let fd = screencast.open_pipe_wire_remote(&session).await.map_err(err)?;
            let token = selected.restore_token().map(str::to_string);
            let node = stream.pipe_wire_node_id();
            let (tx, mut rx) = mpsc::unbounded_channel::<PortalInput>();
            tokio::spawn(async move {
                let mut stopped = stopped;
                loop {
                    tokio::select! {
                        _ = &mut stopped => break,
                        msg = rx.recv() => {
                            let Some(msg) = msg else { break };
                            let state = |down: bool| if down { KeyState::Pressed } else { KeyState::Released };
                            let r = match msg {
                                PortalInput::Motion { x, y } => rd.notify_pointer_motion_absolute(&session, node, x, y).await,
                                PortalInput::Delta { dx, dy } => rd.notify_pointer_motion(&session, dx, dy).await,
                                PortalInput::Button { code, down } => rd.notify_pointer_button(&session, code, state(down)).await,
                                PortalInput::Scroll { dx, dy } => {
                                    let r = rd.notify_pointer_axis(&session, dx, dy, true).await;
                                    if r.is_err() && dy != 0.0 {
                                        rd.notify_pointer_axis_discrete(&session, Axis::Vertical, if dy > 0.0 { 1 } else { -1 }).await
                                    } else {
                                        r
                                    }
                                }
                                PortalInput::Key { code, down } => rd.notify_keyboard_keycode(&session, code, state(down)).await,
                            };
                            if let Err(e) = r {
                                crate::inject::failed(&format!("portal: {e}"));
                            }
                        }
                    }
                }
                let _ = session.close().await;
                tracing::info!("portal: screen-sharing session closed");
            });
            Ok(Started { fd, node, size: stream.size(), token, input: Some(tx), _stop: stop })
        }
        _ => {
            // No RemoteDesktop portal (xdg-desktop-portal-wlr): the picture only.
            tracing::info!(virtual_display, saved = token.is_some(), "portal: ScreenCast only (no RemoteDesktop)");
            let session = screencast.create_session().await.map_err(err)?;
            screencast
                .select_sources(
                    &session,
                    CursorMode::Embedded,
                    source.into(),
                    false,
                    token.as_deref(),
                    PersistMode::ExplicitlyRevoked,
                )
                .await
                .map_err(err)?;
            let streams = screencast.start(&session, None).await.map_err(err)?.response().map_err(err)?;
            let stream = streams.streams().first().ok_or("The desktop shared no screen.")?.clone();
            let fd = screencast.open_pipe_wire_remote(&session).await.map_err(err)?;
            let token = streams.restore_token().map(str::to_string);
            tokio::spawn(async move {
                let _ = stopped.await;
                let _ = session.close().await;
                tracing::info!("portal: screen-sharing session closed");
            });
            Ok(Started { fd, node: stream.pipe_wire_node_id(), size: stream.size(), token, input: None, _stop: stop })
        }
    }
}

impl Started {
    /// The GStreamer source for this screen.
    pub fn source(&self) -> String {
        use std::os::fd::AsRawFd;
        format!(
            "pipewiresrc fd={} path={} do-timestamp=true keepalive-time=500 always-copy=true",
            self.fd.as_raw_fd(),
            self.node
        )
    }
}
