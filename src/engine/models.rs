//! Data types shared by the tracker, the database and the dashboard.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// A Steam library entry discovered from an `appmanifest_*.acf` file.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Game {
    pub appid: u32,
    pub name: String,
    /// Root of the Steam library that owns this install.
    pub library: Option<PathBuf>,
    /// `<library>/steamapps/common/<installdir>`.
    pub install_dir: Option<PathBuf>,
    pub size_on_disk: u64,
    pub installed: bool,
    /// Unix timestamp from the manifest (0 / missing when never played).
    pub last_played: Option<i64>,
    /// Set by the tracker each poll, never persisted.
    pub running: bool,
}

/// Where a tracked session came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SessionSource {
    /// Observed by the process tracker.
    Tracker,
    /// Started by a launch queue run.
    Queue,
}

impl SessionSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Tracker => "tracker",
            Self::Queue => "queue",
        }
    }

    pub fn parse(value: &str) -> Self {
        match value {
            "queue" => Self::Queue,
            _ => Self::Tracker,
        }
    }
}

/// One tracked interval of an actually running game.
#[derive(Debug, Clone)]
pub struct Session {
    pub id: i64,
    pub appid: u32,
    pub game_name: String,
    pub started_at: i64,
    pub ended_at: Option<i64>,
    pub duration_secs: i64,
    pub source: SessionSource,
}

impl Session {
    /// Duration including the still-open part of a live session.
    pub fn live_duration(&self, now: i64) -> i64 {
        match self.ended_at {
            Some(_) => self.duration_secs.max(0),
            None => (now - self.started_at).max(0),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct Stats {
    pub total_secs: i64,
    pub sessions: i64,
    pub games: i64,
    pub last_24h_secs: i64,
}

#[derive(Debug, Clone)]
pub struct QueueItem {
    pub id: i64,
    pub appid: u32,
    pub game_name: String,
    pub minutes: u32,
}

#[derive(Debug, Clone)]
pub struct Queue {
    pub id: i64,
    pub name: String,
    pub items: Vec<QueueItem>,
}

/// Persisted user preferences (stored as JSON in the local database).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Register a launch entry for the current user. Off by default.
    pub start_with_windows: bool,
    /// Hide the window instead of exiting when it is closed.
    pub close_to_tray: bool,
    /// How often running game processes are sampled, in seconds.
    pub poll_seconds: u32,
    /// Dashboard theme. The MVP ships a dark theme and keeps this switch.
    pub dark_theme: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            start_with_windows: false,
            close_to_tray: true,
            poll_seconds: 5,
            dark_theme: true,
        }
    }
}

impl Settings {
    pub fn sanitized(mut self) -> Self {
        self.poll_seconds = self.poll_seconds.clamp(2, 60);
        self
    }
}

/// What we know about the local Steam client right now.
#[derive(Debug, Clone, Default)]
pub struct SteamStatus {
    pub installed: bool,
    pub running: bool,
    pub root: Option<PathBuf>,
    pub steam_exe: Option<PathBuf>,
    pub libraries: Vec<PathBuf>,
}

/// Human friendly duration, e.g. `2h 07m`, `14m 32s`, `42s`.
pub fn format_duration(total_secs: i64) -> String {
    let total = total_secs.max(0);
    let hours = total / 3600;
    let minutes = (total % 3600) / 60;
    let seconds = total % 60;
    if hours > 0 {
        format!("{hours}h {minutes:02}m")
    } else if minutes > 0 {
        format!("{minutes}m {seconds:02}s")
    } else {
        format!("{seconds}s")
    }
}

/// Human friendly size, e.g. `48.2 GB`.
pub fn format_size(bytes: u64) -> String {
    const GB: f64 = 1024.0 * 1024.0 * 1024.0;
    const MB: f64 = 1024.0 * 1024.0;
    let bytes = bytes as f64;
    if bytes >= GB {
        format!("{:.1} GB", bytes / GB)
    } else if bytes >= MB {
        format!("{:.0} MB", bytes / MB)
    } else {
        "—".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duration_formatting() {
        assert_eq!(format_duration(0), "0s");
        assert_eq!(format_duration(42), "42s");
        assert_eq!(format_duration(14 * 60 + 32), "14m 32s");
        assert_eq!(format_duration(2 * 3600 + 7 * 60), "2h 07m");
        assert_eq!(format_duration(-10), "0s");
    }

    #[test]
    fn live_session_uses_now_when_open() {
        let session = Session {
            id: 1,
            appid: 440,
            game_name: "Team Fortress 2".into(),
            started_at: 1_000,
            ended_at: None,
            duration_secs: 0,
            source: SessionSource::Tracker,
        };
        assert_eq!(session.live_duration(1_065), 65);
    }

    #[test]
    fn settings_are_clamped() {
        let settings = Settings {
            poll_seconds: 1000,
            ..Settings::default()
        }
        .sanitized();
        assert_eq!(settings.poll_seconds, 60);
    }
}
