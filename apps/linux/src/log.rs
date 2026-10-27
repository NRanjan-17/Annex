//! Logging: to stderr always, and to `annex.log` in the data folder when Settings → Save a log
//! is on (`RUST_LOG` picks the level, default info).

use std::path::Path;
use std::sync::Mutex;

pub fn init(data_dir: &Path, save: bool) {
    let filter = std::env::var("RUST_LOG").ok().and_then(|s| s.parse().ok()).unwrap_or(tracing::Level::INFO);
    let file = save
        .then(|| {
            let _ = std::fs::create_dir_all(data_dir);
            std::fs::OpenOptions::new().create(true).append(true).open(data_dir.join("annex.log")).ok()
        })
        .flatten();
    let builder = tracing_subscriber::fmt().with_max_level(filter).with_target(false);
    let _ = match file {
        Some(f) => builder.with_ansi(false).with_writer(Mutex::new(f)).try_init(),
        None => builder.with_writer(std::io::stderr).try_init(),
    };
}
