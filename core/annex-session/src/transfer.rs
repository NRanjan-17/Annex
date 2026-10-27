//! Clipboard items and files between the two computers in a session. Each goes on its own QUIC
//! stream inside the session's encrypted connection, so it never holds up the video or input,
//! and never passes through a server (a relay only forwards the encrypted packets).
//!
//! A stream carries: the header's length (u32, big-endian), the header (JSON), `size` bytes, then
//! the SHA-256 of those bytes. Files are written to a hidden `.part` file in the downloads folder
//! and get their real name only once the checksum matches; a failed, refused or cancelled one is
//! deleted, so nothing half-written is left behind. The log records outcomes, never names or
//! contents.
//!
//! Both sides must say [`CAP`] in their hello, and the session must run over QUIC. Through our
//! relay (which has a monthly allowance) everything is paced to [`LIMITED_RATE`], and clipboard
//! items are held to [`MAX_CLIPBOARD_LIMITED`].

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::events::{Event, EventSink};

/// Why a session can't send files.
pub(crate) fn no_transfers() -> String {
    "The other computer can't take files: update Annex there, or connect without the TCP fallback.".into()
}

/// Said in the hello by peers that take transfers.
pub const CAP: &str = "transfer/1";
/// Largest clipboard item (bigger ones should go as files).
pub const MAX_CLIPBOARD: u64 = 32 << 20;
/// Largest clipboard item through our relay.
pub const MAX_CLIPBOARD_LIMITED: u64 = 1 << 20;
/// Bytes per second through our relay: 1 Mbit/s on top of the session's video ceiling keeps a
/// month's worst case under the relay's free allowance (server/rendezvous/README.md).
pub const LIMITED_RATE: u64 = 125_000;
const MAX_HEADER: u32 = 4096;
const MAX_NAME: usize = 200;
const CHUNK: usize = 256 << 10;
const LIMITED_CHUNK: usize = 16 << 10;
const PROGRESS_EVERY: Duration = Duration::from_millis(250);

/// Stream stop codes.
const CANCELLED: u32 = 1;
const REFUSED: u32 = 2;
const FAILED: u32 = 3;

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Item {
    /// `mime`: "text/plain;charset=utf-8" or "image/png".
    Clipboard {
        mime: String,
    },
    File {
        name: String,
    },
}

#[derive(Serialize, Deserialize, Debug)]
struct Header {
    id: u64,
    #[serde(flatten)]
    item: Item,
    size: u64,
}

/// One session's transfers, both ways.
pub struct Transfers {
    conn: quinn::Connection,
    session_id: u64,
    events: EventSink,
    /// Where received files go; None: files are refused.
    downloads: Option<PathBuf>,
    /// Through our relay: paced and capped.
    limited: bool,
    /// Whether the other side may send to this one right now (re-checked per chunk).
    allow_incoming: Box<dyn Fn() -> bool + Send + Sync>,
    next_id: AtomicU64,
    /// Cancel flags of running transfers, by (incoming, id).
    running: Mutex<HashMap<(bool, u64), Arc<AtomicBool>>>,
    tasks: Mutex<tokio::task::JoinSet<()>>,
}

impl Transfers {
    pub(crate) fn start(
        conn: quinn::Connection,
        session_id: u64,
        events: EventSink,
        downloads: Option<PathBuf>,
        limited: bool,
        allow_incoming: Box<dyn Fn() -> bool + Send + Sync>,
    ) -> Arc<Self> {
        let t = Arc::new(Transfers {
            conn,
            session_id,
            events,
            downloads,
            limited,
            allow_incoming,
            next_id: AtomicU64::new(1),
            running: Mutex::new(HashMap::new()),
            tasks: Mutex::new(tokio::task::JoinSet::new()),
        });
        let me = t.clone();
        t.tasks.lock().unwrap().spawn(async move {
            while let Ok(recv) = me.conn.accept_uni().await {
                let me2 = me.clone();
                me.tasks.lock().unwrap().spawn(async move { me2.receive(recv).await });
            }
        });
        t
    }

    /// Ends every transfer (the session is over); partial files are removed by their tasks.
    pub(crate) fn close(&self) {
        for flag in self.running.lock().unwrap().values() {
            flag.store(true, Ordering::Relaxed);
        }
        self.tasks.lock().unwrap().abort_all();
    }

    /// Sends a clipboard item. False if it's too big for this link.
    pub fn send_clipboard(self: &Arc<Self>, mime: &str, data: Vec<u8>) -> bool {
        let cap = if self.limited { MAX_CLIPBOARD_LIMITED } else { MAX_CLIPBOARD };
        if data.len() as u64 > cap {
            return false;
        }
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let header = Header { id, item: Item::Clipboard { mime: mime.into() }, size: data.len() as u64 };
        let me = self.clone();
        self.tasks.lock().unwrap().spawn(async move {
            let flag = Arc::new(AtomicBool::new(false));
            let mut src: &[u8] = &data;
            if let Err(e) = me.send(&header, &mut src, &flag, |_| {}).await {
                tracing::debug!("clipboard not sent: {e}");
            }
        });
        true
    }

    /// Sends a file; progress and the outcome come as transfer events with the returned id.
    pub fn send_file(self: &Arc<Self>, path: &Path) -> Result<u64, String> {
        let name = path.file_name().and_then(|n| n.to_str()).ok_or("That file has no name.")?.to_string();
        let size = std::fs::metadata(path).map_err(|e| e.to_string())?.len();
        if std::fs::metadata(path).is_ok_and(|m| m.is_dir()) {
            return Err("Send the files in a folder, not the folder itself.".into());
        }
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let flag = Arc::new(AtomicBool::new(false));
        self.running.lock().unwrap().insert((false, id), flag.clone());
        (self.events)(Event::TransferStarted {
            session_id: self.session_id,
            id,
            incoming: false,
            name: name.clone(),
            size,
        });
        let (me, path) = (self.clone(), path.to_path_buf());
        self.tasks.lock().unwrap().spawn(async move {
            let header = Header { id, item: Item::File { name }, size };
            let result = match tokio::fs::File::open(&path).await {
                Ok(mut f) => {
                    let events = me.events.clone();
                    let sid = me.session_id;
                    me.send(&header, &mut f, &flag, move |done| {
                        events(Event::TransferProgress { session_id: sid, id, incoming: false, done })
                    })
                    .await
                }
                Err(e) => Err(e.to_string()),
            };
            me.running.lock().unwrap().remove(&(false, id));
            tracing::info!(id, ok = result.is_ok(), "file sent");
            me.ended(false, id, result.map(|()| None));
        });
        Ok(id)
    }

    /// Stops a running transfer, either way. The receiving side deletes what it had written.
    pub fn cancel(&self, incoming: bool, id: u64) {
        if let Some(flag) = self.running.lock().unwrap().get(&(incoming, id)) {
            flag.store(true, Ordering::Relaxed);
        }
    }

    fn ended(&self, incoming: bool, id: u64, result: Result<Option<PathBuf>, String>) {
        let (ok, path, error) = match result {
            Ok(p) => (true, p.map(|p| p.to_string_lossy().into_owned()), None),
            Err(e) => (false, None, Some(e)),
        };
        (self.events)(Event::TransferEnded { session_id: self.session_id, id, incoming, ok, path, error });
    }

    async fn send<R: tokio::io::AsyncRead + Unpin>(
        &self,
        header: &Header,
        src: &mut R,
        cancel: &AtomicBool,
        progress: impl Fn(u64),
    ) -> Result<(), String> {
        let mut out = self.conn.open_uni().await.map_err(|e| e.to_string())?;
        let head = serde_json::to_vec(header).map_err(|e| e.to_string())?;
        out.write_all(&(head.len() as u32).to_be_bytes()).await.map_err(stopped)?;
        out.write_all(&head).await.map_err(stopped)?;
        let mut hash = ring::digest::Context::new(&ring::digest::SHA256);
        let mut buf = vec![0u8; if self.limited { LIMITED_CHUNK } else { CHUNK }];
        let (mut done, started) = (0u64, Instant::now());
        let mut last = Instant::now();
        while done < header.size {
            if cancel.load(Ordering::Relaxed) {
                let _ = out.reset(CANCELLED.into());
                return Err("Cancelled.".into());
            }
            let want = buf.len().min((header.size - done) as usize);
            let n = src.read(&mut buf[..want]).await.map_err(|e| e.to_string())?;
            if n == 0 {
                let _ = out.reset(FAILED.into());
                return Err("The file got shorter while it was being sent.".into());
            }
            hash.update(&buf[..n]);
            out.write_all(&buf[..n]).await.map_err(stopped)?;
            done += n as u64;
            if self.limited {
                // Packet by packet at the relay's pace.
                let due = Duration::from_secs_f64(done as f64 / LIMITED_RATE as f64);
                if let Some(wait) = due.checked_sub(started.elapsed()) {
                    tokio::time::sleep(wait).await;
                }
            }
            if last.elapsed() >= PROGRESS_EVERY {
                last = Instant::now();
                progress(done);
            }
        }
        out.write_all(hash.finish().as_ref()).await.map_err(stopped)?;
        out.finish().map_err(|e| e.to_string())?;
        match out.stopped().await {
            Ok(None) => Ok(()),
            Ok(Some(code)) => Err(stop_reason(code.into_inner() as u32)),
            Err(e) => Err(e.to_string()),
        }
    }

    async fn receive(self: Arc<Self>, mut recv: quinn::RecvStream) {
        let header = match read_header(&mut recv).await {
            Ok(h) => h,
            Err(e) => {
                tracing::debug!("transfer: bad header: {e}");
                let _ = recv.stop(FAILED.into());
                return;
            }
        };
        if !(self.allow_incoming)() {
            tracing::info!("transfer refused: this computer doesn't take them from the other one now");
            let _ = recv.stop(REFUSED.into());
            return;
        }
        match header.item {
            Item::Clipboard { mime } => {
                let cap = if self.limited { MAX_CLIPBOARD_LIMITED } else { MAX_CLIPBOARD };
                if header.size > cap {
                    let _ = recv.stop(REFUSED.into());
                    return;
                }
                let mut data = vec![0u8; header.size as usize];
                let mut sum = [0u8; 32];
                let read = async {
                    recv.read_exact(&mut data).await?;
                    recv.read_exact(&mut sum).await
                };
                if read.await.is_err() || ring::digest::digest(&ring::digest::SHA256, &data).as_ref() != sum {
                    tracing::debug!(%mime, "clipboard item damaged; dropped");
                    let _ = recv.stop(FAILED.into());
                    return;
                }
                tracing::debug!(%mime, "clipboard item received");
                (self.events)(Event::ClipboardReceived { session_id: self.session_id, mime, data });
            }
            Item::File { name } => {
                let Some(dir) = self.downloads.clone() else {
                    let _ = recv.stop(REFUSED.into());
                    return;
                };
                let name = safe_name(&name);
                let id = header.id;
                let flag = Arc::new(AtomicBool::new(false));
                self.running.lock().unwrap().insert((true, id), flag.clone());
                (self.events)(Event::TransferStarted {
                    session_id: self.session_id,
                    id,
                    incoming: true,
                    name: name.clone(),
                    size: header.size,
                });
                let result = self.receive_file(&mut recv, &dir, &name, header.size, id, &flag).await;
                self.running.lock().unwrap().remove(&(true, id));
                tracing::info!(id, ok = result.is_ok(), "file received");
                self.ended(true, id, result.map(Some));
            }
        }
    }

    async fn receive_file(
        &self,
        recv: &mut quinn::RecvStream,
        dir: &Path,
        name: &str,
        size: u64,
        id: u64,
        cancel: &AtomicBool,
    ) -> Result<PathBuf, String> {
        tokio::fs::create_dir_all(dir).await.map_err(|e| e.to_string())?;
        let part = dir.join(format!(".annex-{}-{id}.part", std::process::id()));
        // Removes the partial file on every way out but success.
        struct Part<'a>(&'a Path, bool);
        impl Drop for Part<'_> {
            fn drop(&mut self) {
                if !self.1 {
                    let _ = std::fs::remove_file(self.0);
                }
            }
        }
        let mut guard = Part(&part, false);
        let mut file = open_part(&part).await.map_err(|e| e.to_string())?;
        let mut hash = ring::digest::Context::new(&ring::digest::SHA256);
        let mut buf = vec![0u8; CHUNK];
        let (mut done, mut last) = (0u64, Instant::now());
        while done < size {
            if cancel.load(Ordering::Relaxed) || !(self.allow_incoming)() {
                let _ = recv.stop(CANCELLED.into());
                return Err("Cancelled.".into());
            }
            let want = buf.len().min((size - done) as usize);
            let n = match recv.read(&mut buf[..want]).await {
                Ok(Some(n)) => n,
                Ok(None) => return Err("The other computer stopped sending.".into()),
                Err(e) => return Err(read_reason(e)),
            };
            hash.update(&buf[..n]);
            file.write_all(&buf[..n]).await.map_err(|e| {
                let _ = recv.stop(FAILED.into());
                e.to_string()
            })?;
            done += n as u64;
            if last.elapsed() >= PROGRESS_EVERY {
                last = Instant::now();
                (self.events)(Event::TransferProgress { session_id: self.session_id, id, incoming: true, done });
            }
        }
        let mut sum = [0u8; 32];
        recv.read_exact(&mut sum).await.map_err(|_| "The file didn't arrive whole.".to_string())?;
        if hash.finish().as_ref() != sum {
            return Err("The file arrived damaged and was deleted.".into());
        }
        file.sync_all().await.map_err(|e| e.to_string())?;
        drop(file);
        let dest = unique_path(dir, name);
        tokio::fs::rename(&part, &dest).await.map_err(|e| e.to_string())?;
        #[cfg(windows)]
        unhide(&dest);
        guard.1 = true;
        Ok(dest)
    }
}

async fn read_header(recv: &mut quinn::RecvStream) -> Result<Header, String> {
    let mut len = [0u8; 4];
    recv.read_exact(&mut len).await.map_err(|e| e.to_string())?;
    let len = u32::from_be_bytes(len);
    if len == 0 || len > MAX_HEADER {
        return Err(format!("header of {len} bytes"));
    }
    let mut head = vec![0u8; len as usize];
    recv.read_exact(&mut head).await.map_err(|e| e.to_string())?;
    serde_json::from_slice(&head).map_err(|e| e.to_string())
}

/// The partial file: readable only by this account, and hidden on Windows (on Mac and Linux
/// the leading dot hides it).
async fn open_part(path: &Path) -> std::io::Result<tokio::fs::File> {
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_ATTRIBUTE_HIDDEN: u32 = 0x2;
        opts.attributes(FILE_ATTRIBUTE_HIDDEN);
    }
    Ok(tokio::fs::File::from_std(opts.open(path)?))
}

#[cfg(windows)]
fn unhide(path: &Path) {
    // The finished file keeps the partial file's hidden attribute; clear it.
    let _ = std::process::Command::new("attrib").arg("-h").arg(path).status();
}

/// A name that stays inside the downloads folder: no folders, no control characters, no
/// reserved names, at most [`MAX_NAME`] bytes.
pub fn safe_name(name: &str) -> String {
    let base = name.rsplit(['/', '\\']).next().unwrap_or("");
    let mut s: String = base
        .chars()
        .map(|c| if c.is_control() || matches!(c, ':' | '*' | '?' | '"' | '<' | '>' | '|') { '_' } else { c })
        .collect();
    s = s.trim().trim_end_matches('.').to_string();
    while s.len() > MAX_NAME {
        s.pop();
    }
    let stem = s.split('.').next().unwrap_or("").to_ascii_uppercase();
    const RESERVED: [&str; 6] = ["CON", "PRN", "AUX", "NUL", "COM1", "LPT1"];
    if s.is_empty() || s.starts_with('.') && s.trim_start_matches('.').is_empty() || RESERVED.contains(&stem.as_str()) {
        s = format!("file {s}").trim().to_string();
    }
    s
}

/// `dir/name`, or `name (2).ext`, `name (3).ext`… if it's taken.
fn unique_path(dir: &Path, name: &str) -> PathBuf {
    let first = dir.join(name);
    if !first.exists() {
        return first;
    }
    let (stem, ext) = match name.rfind('.') {
        Some(i) if i > 0 => (&name[..i], &name[i..]),
        _ => (name, ""),
    };
    (2..).map(|n| dir.join(format!("{stem} ({n}){ext}"))).find(|p| !p.exists()).expect("a free name")
}

fn stop_reason(code: u32) -> String {
    match code {
        CANCELLED => "Cancelled on the other computer.".into(),
        REFUSED => "The other computer doesn't take files from this one now.".into(),
        _ => "The other computer couldn't save it.".into(),
    }
}

fn stopped(e: quinn::WriteError) -> String {
    match e {
        quinn::WriteError::Stopped(code) => stop_reason(code.into_inner() as u32),
        other => other.to_string(),
    }
}

fn read_reason(e: quinn::ReadError) -> String {
    match e {
        quinn::ReadError::Reset(code) if code.into_inner() as u32 == CANCELLED => {
            "Cancelled on the other computer.".into()
        }
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_stay_in_the_folder() {
        assert_eq!(safe_name("../../etc/passwd"), "passwd");
        assert_eq!(safe_name(r"C:\Users\x\report.pdf"), "report.pdf");
        assert_eq!(safe_name("a:b*c?.txt"), "a_b_c_.txt");
        assert_eq!(safe_name(".."), "file");
        assert_eq!(safe_name("CON.txt"), "file CON.txt");
        assert_eq!(safe_name(""), "file");
        assert!(safe_name(&"x".repeat(500)).len() <= MAX_NAME);
    }

    #[test]
    fn taken_names_get_a_number() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), b"1").unwrap();
        std::fs::write(dir.path().join("a (2).txt"), b"2").unwrap();
        assert_eq!(unique_path(dir.path(), "a.txt"), dir.path().join("a (3).txt"));
        assert_eq!(unique_path(dir.path(), "b"), dir.path().join("b"));
    }
}
