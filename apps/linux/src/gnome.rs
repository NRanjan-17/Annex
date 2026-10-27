//! GNOME: Mutter's own screen-cast and remote-desktop service (what GNOME's Remote Desktop
//! uses). It records a monitor, or a new virtual monitor for Extend, and takes the viewer's
//! mouse and keyboard, without a portal dialog: the user turned sharing on in Annex.

use std::collections::HashMap;

use futures_util::StreamExt;
use tokio::sync::mpsc;
use zbus::zvariant::{OwnedObjectPath, Value};

use crate::portal::PortalInput;

const RD: &str = "org.gnome.Mutter.RemoteDesktop";
const SC: &str = "org.gnome.Mutter.ScreenCast";

pub struct Started {
    pub node: u32,
    pub input: mpsc::UnboundedSender<PortalInput>,
    _stop: tokio::sync::oneshot::Sender<()>,
}

impl Started {
    pub fn source(&self, size: Option<(u32, u32)>) -> String {
        // A virtual monitor takes the size the stream asks for. (Mutter offers a frame-rate
        // range; fixing one fails to negotiate. The encoder side caps the rate.)
        let caps = match size {
            Some((w, h)) => format!(" ! video/x-raw,width={w},height={h}"),
            None => String::new(),
        };
        format!("pipewiresrc path={} do-timestamp=true keepalive-time=500 always-copy=true{caps}", self.node)
    }
}

fn err(e: zbus::Error) -> String {
    format!("GNOME's screen sharing failed: {e}")
}

/// `virtual_display`: record a new monitor (Extend) instead of the primary one.
pub async fn start(virtual_display: bool) -> Result<Started, String> {
    tracing::info!(virtual_display, "GNOME: starting a Mutter screen cast");
    let conn = zbus::Connection::session().await.map_err(err)?;
    let rd = zbus::Proxy::new(&conn, RD, "/org/gnome/Mutter/RemoteDesktop", RD).await.map_err(err)?;
    let rd_path: OwnedObjectPath = rd.call("CreateSession", &()).await.map_err(err)?;
    let rd_session = zbus::Proxy::new(&conn, RD, rd_path.clone(), format!("{RD}.Session")).await.map_err(err)?;
    let session_id: String = rd_session.get_property("SessionId").await.map_err(err)?;

    let sc = zbus::Proxy::new(&conn, SC, "/org/gnome/Mutter/ScreenCast", SC).await.map_err(err)?;
    let mut props: HashMap<&str, Value<'_>> = HashMap::new();
    props.insert("remote-desktop-session-id", Value::from(session_id.as_str()));
    let sc_path: OwnedObjectPath = sc.call("CreateSession", &(props,)).await.map_err(err)?;
    let sc_session = zbus::Proxy::new(&conn, SC, sc_path, format!("{SC}.Session")).await.map_err(err)?;

    let mut rec: HashMap<&str, Value<'_>> = HashMap::new();
    // 1: the pointer drawn into the picture.
    rec.insert("cursor-mode", Value::U32(1));
    let stream_path: OwnedObjectPath = if virtual_display {
        rec.insert("is-platform", Value::Bool(true));
        sc_session.call("RecordVirtual", &(rec,)).await.map_err(err)?
    } else {
        // An empty connector: the primary monitor.
        sc_session.call("RecordMonitor", &("", rec)).await.map_err(err)?
    };
    let stream = zbus::Proxy::new(&conn, SC, stream_path.clone(), format!("{SC}.Stream")).await.map_err(err)?;
    let mut added = stream.receive_signal("PipeWireStreamAdded").await.map_err(err)?;

    rd_session.call::<_, _, ()>("Start", &()).await.map_err(err)?;
    let node: u32 = tokio::time::timeout(std::time::Duration::from_secs(10), added.next())
        .await
        .map_err(|_| "GNOME didn't start the screen stream.".to_string())?
        .ok_or("GNOME didn't start the screen stream.")?
        .body()
        .deserialize()
        .map_err(err)?;

    tracing::info!(node, virtual_display, "GNOME: screen stream started");
    let (stop, mut stopped) = tokio::sync::oneshot::channel::<()>();
    let (tx, mut rx) = mpsc::unbounded_channel::<PortalInput>();
    let stream_path = stream_path.to_string();
    tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = &mut stopped => break,
                msg = rx.recv() => {
                    let Some(msg) = msg else { break };
                    let r = match msg {
                        PortalInput::Motion { x, y } => {
                            rd_session.call::<_, _, ()>("NotifyPointerMotionAbsolute", &(stream_path.as_str(), x, y)).await
                        }
                        PortalInput::Delta { dx, dy } => {
                            rd_session.call::<_, _, ()>("NotifyPointerMotionRelative", &(dx, dy)).await
                        }
                        PortalInput::Button { code, down } => {
                            rd_session.call::<_, _, ()>("NotifyPointerButton", &(code, down)).await
                        }
                        // Flag 1: finish (a whole scroll, not a touchpad gesture in progress).
                        PortalInput::Scroll { dx, dy } => {
                            rd_session.call::<_, _, ()>("NotifyPointerAxis", &(dx, dy, 1u32)).await
                        }
                        PortalInput::Key { code, down } => {
                            rd_session.call::<_, _, ()>("NotifyKeyboardKeycode", &(code as u32, down)).await
                        }
                    };
                    if let Err(e) = r {
                        crate::inject::failed(&format!("GNOME: {e}"));
                    }
                }
            }
        }
        if let Err(e) = rd_session.call::<_, _, ()>("Stop", &()).await {
            tracing::debug!("GNOME: stopping the session: {e}");
        }
        tracing::info!("GNOME: screen stream stopped");
    });
    Ok(Started { node, input: tx, _stop: stop })
}
