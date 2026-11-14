//! Unix implementation of the platform layer. This is intentionally small: it
//! keeps the app compilable and functional on Linux for future support, using
//! `/proc` for process discovery and the freedesktop launcher for URLs.

use super::ProcInfo;
use std::path::{Path, PathBuf};
use std::process::Command;

pub struct InstanceGuard;

pub fn single_instance() -> Option<InstanceGuard> {
    Some(InstanceGuard)
}

/// The XDG stub has no taskbar concept yet.
pub fn hide_from_taskbar() {}

pub fn request_show_existing() {}

pub fn spawn_show_listener<F>(_on_show: F)
where
    F: Fn() + Send + 'static,
{
}

pub fn list_processes() -> Vec<ProcInfo> {
    let mut processes = Vec::new();
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return processes;
    };
    for entry in entries.flatten() {
        let file_name = entry.file_name();
        let Ok(pid) = file_name.to_string_lossy().parse::<u32>() else {
            continue;
        };
        let name = std::fs::read_to_string(entry.path().join("comm"))
            .map(|value| value.trim().to_string())
            .unwrap_or_default();
        processes.push(ProcInfo {
            pid,
            name,
            exe: None,
        });
    }
    processes
}

pub fn process_image_path(pid: u32) -> Option<PathBuf> {
    std::fs::read_link(format!("/proc/{pid}/exe")).ok()
}

pub fn open_url(url: &str) -> Result<(), String> {
    Command::new("xdg-open")
        .arg(url)
        .spawn()
        .map(|_| ())
        .map_err(|err| format!("xdg-open: {err}"))
}

pub fn open_path(path: &Path) -> Result<(), String> {
    open_url(&path.to_string_lossy())
}

pub fn spawn_detached(exe: &Path, args: &[&str]) -> Result<(), String> {
    Command::new(exe)
        .args(args)
        .spawn()
        .map(|_| ())
        .map_err(|err| format!("start {}: {err}", exe.display()))
}

pub fn steam_install_candidates() -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(home) = std::env::var_os("HOME") {
        let home = PathBuf::from(home);
        candidates.push(home.join(".steam").join("steam"));
        candidates.push(home.join(".local").join("share").join("Steam"));
        candidates.push(
            home.join(".var")
                .join("app")
                .join("com.valvesoftware.Steam")
                .join(".local")
                .join("share")
                .join("Steam"),
        );
    }
    candidates
}

pub fn request_close(pids: &[u32]) {
    for pid in pids {
        let _ = Command::new("kill").arg(pid.to_string()).status();
    }
}

pub fn terminate(pid: u32) {
    let _ = Command::new("kill").arg("-9").arg(pid.to_string()).status();
}

pub fn fatal_error(message: &str) {
    eprintln!("SteamIdle: {message}");
}
