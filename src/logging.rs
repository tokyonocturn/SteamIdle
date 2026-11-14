//! Minimal file logger. No telemetry, no network: only a rotating local log
//! next to the database, useful when something does not behave as expected.

use chrono::Local;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

const MAX_LOG_BYTES: u64 = 2 * 1024 * 1024;

static LOGGER: OnceLock<Mutex<Option<File>>> = OnceLock::new();

pub fn init(path: PathBuf) {
    rotate_if_needed(&path);
    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .ok();
    let _ = LOGGER.set(Mutex::new(file));
    info(format!("SteamIdle {} starting", env!("CARGO_PKG_VERSION")));
}

fn rotate_if_needed(path: &Path) {
    let too_big = fs::metadata(path)
        .map(|meta| meta.len() > MAX_LOG_BYTES)
        .unwrap_or(false);
    if too_big {
        let _ = fs::remove_file(path.with_extension("log.old"));
        let _ = fs::rename(path, path.with_extension("log.old"));
    }
}

fn write_line(level: &str, message: &str) {
    let line = format!(
        "{} [{level}] {message}\n",
        Local::now().format("%Y-%m-%d %H:%M:%S")
    );
    if let Some(mutex) = LOGGER.get() {
        if let Ok(mut guard) = mutex.lock() {
            if let Some(file) = guard.as_mut() {
                let _ = file.write_all(line.as_bytes());
                let _ = file.flush();
            }
        }
    }
    #[cfg(debug_assertions)]
    eprint!("{line}");
}

pub fn info(message: impl AsRef<str>) {
    write_line("INFO", message.as_ref());
}

pub fn warn(message: impl AsRef<str>) {
    write_line("WARN", message.as_ref());
}

pub fn error(message: impl AsRef<str>) {
    write_line("ERROR", message.as_ref());
}
