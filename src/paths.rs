//! Portable file locations. The database lives next to the executable so the
//! whole app stays one folder you can copy to a USB stick.
//!
//! If the executable folder is read-only (for example when someone drops the
//! exe into Program Files) we fall back to the per-user local app data folder
//! so the app still works instead of failing to start.

use std::fs;
use std::path::{Path, PathBuf};

pub fn portable_dir() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(Path::to_path_buf))
        .unwrap_or_else(|| PathBuf::from("."))
}

pub fn data_dir() -> PathBuf {
    let preferred = portable_dir();
    if is_writable(&preferred) {
        preferred
    } else {
        fallback_dir()
    }
}

pub fn database_file() -> PathBuf {
    data_dir().join("steamidle.db")
}

pub fn log_file() -> PathBuf {
    data_dir().join("steamidle.log")
}

fn is_writable(dir: &Path) -> bool {
    let probe = dir.join(format!(".steamidle-write-probe-{}.tmp", std::process::id()));
    match fs::write(&probe, b"ok") {
        Ok(()) => {
            let _ = fs::remove_file(&probe);
            true
        }
        Err(_) => false,
    }
}

fn fallback_dir() -> PathBuf {
    let base = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    let dir = base.join("SteamIdle");
    let _ = fs::create_dir_all(&dir);
    dir
}
