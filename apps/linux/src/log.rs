//! Logging: to stderr, and always to `annex.log` in the data folder (debug for Annex's own steps,
//! info for the libraries under it; `RUST_LOG=<level>` sets one level for everything). Past 5 MB
//! the file moves to `annex.old.log` and a new one starts.

use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use tracing_subscriber::filter::Targets;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

const MAX: u64 = 5 * 1024 * 1024;

struct LogFile {
    path: PathBuf,
    /// The open file and how many bytes it holds.
    file: Option<(File, u64)>,
}

static LOG: Mutex<Option<LogFile>> = Mutex::new(None);

/// Writes each line to stderr and to the log file.
struct Sink;

impl Write for Sink {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let _ = std::io::stderr().write_all(buf);
        let mut log = LOG.lock().unwrap_or_else(|e| e.into_inner());
        let Some(log) = log.as_mut() else { return Ok(buf.len()) };
        if log.file.as_ref().is_some_and(|(_, len)| *len > MAX) {
            log.file = None;
        }
        if log.file.is_none() {
            if std::fs::metadata(&log.path).is_ok_and(|m| m.len() > MAX) {
                let _ = std::fs::rename(&log.path, log.path.with_file_name("annex.old.log"));
            }
            log.file = std::fs::OpenOptions::new().create(true).append(true).open(&log.path).ok().map(|f| {
                let len = f.metadata().map_or(0, |m| m.len());
                (f, len)
            });
        }
        if let Some((f, len)) = log.file.as_mut()
            && f.write_all(buf).is_ok()
        {
            *len += buf.len() as u64;
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

pub fn init(data_dir: &Path) {
    let _ = std::fs::create_dir_all(data_dir);
    *LOG.lock().unwrap_or_else(|e| e.into_inner()) = Some(LogFile { path: data_dir.join("annex.log"), file: None });
    let levels = match std::env::var("RUST_LOG").ok().and_then(|s| s.parse::<tracing::Level>().ok()) {
        Some(level) => Targets::new().with_default(level),
        None => Targets::new().with_default(tracing::Level::INFO).with_target("annex", tracing::Level::DEBUG),
    };
    let _ = tracing_subscriber::registry()
        .with(tracing_subscriber::fmt::layer().with_writer(|| Sink).with_ansi(false).with_target(false))
        .with(levels)
        .try_init();
}
