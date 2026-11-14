//! Platform-independent application logic: Steam discovery, session tracking,
//! queue scheduling and the SQLite store. UI code lives in `crate::app`.

pub mod autostart;
pub mod db;
pub mod models;
pub mod queue;
pub mod steam;
pub mod vdf;
pub mod worker;

use std::time::{SystemTime, UNIX_EPOCH};

/// Seconds since the Unix epoch. Used for every persisted timestamp.
pub fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0)
}
