//! The pairing exchange on the control channel (see annex_crypto::pairing for the crypto).
//!
//! ```text
//! viewer (user types PIN)                         host (shows PIN)
//!   |---------------- pair_spake{A} -------------->|
//!   |<--------------- pair_spake{B} ---------------|
//!   |---------------- pair_confirm{mac_v} -------->|  host verifies; on mismatch: bye{pairing_failed}
//!   |<--------------- pair_confirm{mac_h} ---------|  viewer verifies
//! ```

use std::time::Duration;

use annex_crypto::display_fingerprint;
use annex_crypto::pairing::{PairRole, Pairing, normalize_pin};
use annex_proto::Control;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::time::timeout;

use crate::conn::{Reader, Writer};
use crate::events::{Event, EventSink};
use crate::{Core, SessionError, TrustPolicy};

/// How long the user has to type / look at the PIN.
const USER_TIMEOUT: Duration = Duration::from_secs(120);
const STEP_TIMEOUT: Duration = Duration::from_secs(10);

pub(crate) struct PairContext<'a> {
    pub core: &'a Core,
    pub events: &'a EventSink,
    pub peer_fp: &'a str,
    pub peer_name: &'a str,
    pub exporter: &'a [u8],
}

fn pin_for_display(pin: &str) -> String {
    format!("{} {}", &pin[..3], &pin[3..])
}

async fn expect<R: AsyncRead + Unpin>(reader: &mut Reader<R>) -> Result<Control, SessionError> {
    match timeout(STEP_TIMEOUT, reader.next_control()).await?? {
        Control::Bye { reason } => Err(SessionError::Rejected(reason)),
        other => Ok(other),
    }
}

async fn fail<W: AsyncWrite + Unpin>(writer: &mut Writer<W>, reason: &str) -> SessionError {
    let _ = writer.send_control(&Control::Bye { reason: reason.into() }).await;
    SessionError::Rejected(reason.into())
}

pub(crate) async fn pair_as_viewer<R, W>(
    cx: PairContext<'_>,
    reader: &mut Reader<R>,
    writer: &mut Writer<W>,
) -> Result<(), SessionError>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    if cx.core.policy() == TrustPolicy::RejectUnknown {
        return Err(fail(writer, "not_trusted").await);
    }
    let (request_id, mut pending) = cx.core.register_request();
    (cx.events)(Event::PairingPinRequired {
        request_id,
        peer_name: cx.peer_name.to_string(),
        fingerprint_display: display_fingerprint(cx.peer_fp),
    });
    let result = async {
        // Wait for the user, but notice if the host gives up or cancels meanwhile.
        let pin = tokio::select! {
            answer = timeout(USER_TIMEOUT, pending.answer()) => match answer {
                Ok(Some(pin)) => pin,
                Ok(None) => return Err(fail(writer, "pairing_cancelled").await),
                Err(_) => return Err(fail(writer, "pairing_timeout").await),
            },
            msg = reader.next_control() => return Err(match msg? {
                Control::Bye { reason } => SessionError::Rejected(reason),
                _ => SessionError::Violation("unexpected message while waiting for PIN".into()),
            }),
        };
        let Some(pin) = normalize_pin(&pin) else {
            return Err(fail(writer, "pairing_failed").await);
        };
        let (pairing, msg) = Pairing::start(PairRole::Viewer, &pin, cx.core.fingerprint(), cx.peer_fp, cx.exporter);
        writer.send_control(&Control::PairSpake { msg }).await?;
        let Control::PairSpake { msg: peer_msg } = expect(reader).await? else {
            return Err(SessionError::Violation("expected pair_spake".into()));
        };
        let confirmation = match pairing.finish(&peer_msg) {
            Ok(c) => c,
            Err(_) => return Err(fail(writer, "pairing_failed").await),
        };
        writer.send_control(&Control::PairConfirm { mac: confirmation.mine().to_vec() }).await?;
        let Control::PairConfirm { mac } = expect(reader).await? else {
            return Err(SessionError::Violation("expected pair_confirm".into()));
        };
        if !confirmation.verify_peer(&mac) {
            return Err(fail(writer, "pairing_failed").await);
        }
        cx.core.trust_peer(cx.peer_fp, cx.peer_name)
    }
    .await;
    (cx.events)(Event::PairingFinished { request_id, success: result.is_ok(), peer_name: cx.peer_name.to_string() });
    result
}

/// A headless computer's PIN, in a file only this user can read, for as long as pairing runs.
/// Holds the path and what was written, so a pairing that ends doesn't delete a newer one's PIN.
struct PinFile(Option<(std::path::PathBuf, String)>);

impl PinFile {
    fn show(core: &crate::Core, pin: &str, peer_name: &str) -> Self {
        let Some(path) = core.pin_file().filter(|_| core.headless()) else { return PinFile(None) };
        let text = format!("{pin}\n\nType this PIN on {peer_name} to pair. It works once, for the next 2 minutes.\n");
        match write_private(&path, &text) {
            Ok(()) => {
                tracing::info!(peer = peer_name, file = %path.display(), "pairing PIN written for a headless computer");
                PinFile(Some((path, text)))
            }
            Err(e) => {
                tracing::warn!(file = %path.display(), "couldn't write the pairing PIN: {e}");
                PinFile(None)
            }
        }
    }
}

impl Drop for PinFile {
    fn drop(&mut self) {
        if let Some((path, text)) = &self.0
            && std::fs::read_to_string(path).is_ok_and(|now| &now == text)
        {
            let _ = std::fs::remove_file(path);
        }
    }
}

fn write_private(path: &std::path::Path, text: &str) -> std::io::Result<()> {
    use std::io::Write;
    let _ = std::fs::remove_file(path);
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    options.open(path)?.write_all(text.as_bytes())
}

pub(crate) async fn pair_as_host<R, W>(
    cx: PairContext<'_>,
    reader: &mut Reader<R>,
    writer: &mut Writer<W>,
) -> Result<(), SessionError>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    if cx.core.policy() == TrustPolicy::RejectUnknown {
        return Err(fail(writer, "not_trusted").await);
    }
    if cx.core.pairing_locked_for().is_some() {
        return Err(fail(writer, "pairing_locked").await);
    }
    let pin = annex_crypto::pairing::generate_pin()?;
    let (request_id, mut pending) = cx.core.register_request();
    (cx.events)(Event::PairingPin {
        request_id,
        pin: pin_for_display(&pin),
        peer_name: cx.peer_name.to_string(),
        fingerprint_display: display_fingerprint(cx.peer_fp),
    });
    let _pin_file = PinFile::show(cx.core, &pin_for_display(&pin), cx.peer_name);
    // Set when the host's user cancels: that isn't the peer's failure.
    let mut cancelled_here = false;
    let result = async {
        let peer_msg = tokio::select! {
            msg = timeout(USER_TIMEOUT + STEP_TIMEOUT, reader.next_control()) => match msg.map_err(|_| SessionError::Timeout("pairing"))?? {
                Control::PairSpake { msg } => msg,
                Control::Bye { reason } => return Err(SessionError::Rejected(reason)),
                _ => return Err(SessionError::Violation("expected pair_spake".into())),
            },
            _ = pending.answer() => {
                cancelled_here = true;
                return Err(fail(writer, "pairing_cancelled").await);
            }
        };
        let (pairing, msg) = Pairing::start(PairRole::Host, &pin, cx.peer_fp, cx.core.fingerprint(), cx.exporter);
        writer.send_control(&Control::PairSpake { msg }).await?;
        let Ok(confirmation) = pairing.finish(&peer_msg) else {
            return Err(fail(writer, "pairing_failed").await);
        };
        let Control::PairConfirm { mac } = expect(reader).await? else {
            return Err(SessionError::Violation("expected pair_confirm".into()));
        };
        if !confirmation.verify_peer(&mac) {
            return Err(fail(writer, "pairing_failed").await);
        }
        writer.send_control(&Control::PairConfirm { mac: confirmation.mine().to_vec() }).await?;
        cx.core.trust_peer(cx.peer_fp, cx.peer_name)
    }
    .await;
    // Every run that doesn't pair counts towards the lockout, including ones the peer abandons
    // (bye, hang-up, timeout): otherwise a peer could hold a session slot with a PIN on screen
    // for two minutes per connection, again and again, without ever being locked out.
    if result.is_err() && !cancelled_here {
        cx.core.record_pairing_failure();
    }
    (cx.events)(Event::PairingFinished { request_id, success: result.is_ok(), peer_name: cx.peer_name.to_string() });
    result
}
