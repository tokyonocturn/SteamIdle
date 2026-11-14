//! Local SQLite store. One file next to the executable, WAL for safe
//! crash recovery, and no network access whatsoever.

use crate::engine::models::{Game, Queue, QueueItem, Session, SessionSource, Settings, Stats};
use crate::engine::unix_now;
use rusqlite::{params, Connection, OptionalExtension, Row};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

const SETTINGS_KEY: &str = "settings";

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS games (
    appid         INTEGER PRIMARY KEY,
    name          TEXT    NOT NULL,
    library_path  TEXT,
    install_dir   TEXT,
    size_on_disk  INTEGER NOT NULL DEFAULT 0,
    installed     INTEGER NOT NULL DEFAULT 0,
    last_played   INTEGER,
    updated_at    INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS sessions (
    id             INTEGER PRIMARY KEY AUTOINCREMENT,
    appid          INTEGER NOT NULL,
    game_name      TEXT    NOT NULL,
    started_at     INTEGER NOT NULL,
    ended_at       INTEGER,
    last_heartbeat INTEGER NOT NULL,
    duration_secs  INTEGER NOT NULL DEFAULT 0,
    source         TEXT    NOT NULL DEFAULT 'tracker'
);

CREATE INDEX IF NOT EXISTS idx_sessions_started ON sessions(started_at DESC);
CREATE INDEX IF NOT EXISTS idx_sessions_open ON sessions(ended_at) WHERE ended_at IS NULL;
CREATE INDEX IF NOT EXISTS idx_sessions_appid ON sessions(appid);

CREATE TABLE IF NOT EXISTS queues (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    name       TEXT    NOT NULL UNIQUE,
    created_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS queue_items (
    id        INTEGER PRIMARY KEY AUTOINCREMENT,
    queue_id  INTEGER NOT NULL REFERENCES queues(id) ON DELETE CASCADE,
    position  INTEGER NOT NULL,
    appid     INTEGER NOT NULL,
    game_name TEXT    NOT NULL,
    minutes   INTEGER NOT NULL DEFAULT 30
);

CREATE TABLE IF NOT EXISTS settings (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

PRAGMA user_version = 1;
"#;

pub struct Db {
    conn: Connection,
    path: PathBuf,
}

impl Db {
    pub fn open(path: &Path) -> Result<Self, String> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .map_err(|err| format!("create data folder {}: {err}", parent.display()))?;
        }
        let conn =
            Connection::open(path).map_err(|err| format!("open {}: {err}", path.display()))?;
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;
             PRAGMA foreign_keys = ON;
             PRAGMA busy_timeout = 2000;",
        )
        .map_err(|err| format!("configure database: {err}"))?;

        let db = Self {
            conn,
            path: path.to_path_buf(),
        };
        db.conn
            .execute_batch(SCHEMA)
            .map_err(|err| format!("migrate database: {err}"))?;
        Ok(db)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Flush the write-ahead log when the app exits normally.
    pub fn flush(&self) {
        let _ = self.conn.execute_batch("PRAGMA wal_checkpoint(PASSIVE);");
    }

    // ----------------------------------------------------------------- games

    pub fn upsert_games(&mut self, games: &[Game]) -> Result<(), String> {
        let now = unix_now();
        let tx = self.conn.transaction().map_err(db_error)?;
        {
            let mut stmt = tx
                .prepare(
                    "INSERT INTO games
                        (appid, name, library_path, install_dir, size_on_disk, installed, last_played, updated_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
                     ON CONFLICT(appid) DO UPDATE SET
                        name = excluded.name,
                        library_path = excluded.library_path,
                        install_dir = excluded.install_dir,
                        size_on_disk = excluded.size_on_disk,
                        installed = excluded.installed,
                        last_played = excluded.last_played,
                        updated_at = excluded.updated_at",
                )
                .map_err(db_error)?;
            for game in games {
                stmt.execute(params![
                    game.appid,
                    game.name,
                    game.library.as_ref().map(path_string),
                    game.install_dir.as_ref().map(path_string),
                    game.size_on_disk as i64,
                    game.installed as i64,
                    game.last_played,
                    now,
                ])
                .map_err(db_error)?;
            }
        }
        tx.commit().map_err(db_error)
    }

    // -------------------------------------------------------------- sessions

    pub fn start_session(
        &mut self,
        appid: u32,
        game_name: &str,
        at: i64,
        source: SessionSource,
    ) -> Result<i64, String> {
        self.conn
            .execute(
                "INSERT INTO sessions (appid, game_name, started_at, last_heartbeat, duration_secs, source)
                 VALUES (?1, ?2, ?3, ?3, 0, ?4)",
                params![appid, game_name, at, source.as_str()],
            )
            .map_err(db_error)?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn heartbeat(&self, session_id: i64, at: i64) -> Result<(), String> {
        self.conn
            .execute(
                "UPDATE sessions SET last_heartbeat = ?1 WHERE id = ?2 AND ended_at IS NULL",
                params![at, session_id],
            )
            .map_err(db_error)?;
        Ok(())
    }

    pub fn end_session(&self, session_id: i64, at: i64) -> Result<(), String> {
        self.conn
            .execute(
                "UPDATE sessions
                    SET ended_at = ?1,
                        last_heartbeat = MAX(last_heartbeat, ?1),
                        duration_secs = MAX(0, ?1 - started_at)
                  WHERE id = ?2 AND ended_at IS NULL",
                params![at, session_id],
            )
            .map_err(db_error)?;
        Ok(())
    }

    /// Close sessions that were still open when the app last stopped, using
    /// their last heartbeat. This is what makes a crash or a hard power-off
    /// recoverable: we never invent playtime, we only keep what was observed.
    pub fn recover_dangling_sessions(&mut self) -> Result<usize, String> {
        let rows: Vec<(i64, i64, i64)> = {
            let mut stmt = self
                .conn
                .prepare(
                    "SELECT id, started_at, last_heartbeat
                       FROM sessions WHERE ended_at IS NULL",
                )
                .map_err(db_error)?;
            let mapped = stmt
                .query_map([], |row| {
                    Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?, row.get::<_, i64>(2)?))
                })
                .map_err(db_error)?;
            mapped.collect::<Result<Vec<_>, _>>().map_err(db_error)?
        };

        for (id, started_at, last_heartbeat) in &rows {
            let ended_at = (*last_heartbeat).max(*started_at);
            self.conn
                .execute(
                    "UPDATE sessions
                        SET ended_at = ?1,
                            duration_secs = MAX(0, ?1 - started_at)
                      WHERE id = ?2",
                    params![ended_at, id],
                )
                .map_err(db_error)?;
        }
        Ok(rows.len())
    }

    pub fn recent_sessions(&self, limit: u32) -> Result<Vec<Session>, String> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT id, appid, game_name, started_at, ended_at, duration_secs, source
                   FROM sessions
                  ORDER BY started_at DESC, id DESC
                  LIMIT ?1",
            )
            .map_err(db_error)?;
        let rows = stmt
            .query_map(params![limit], session_from_row)
            .map_err(db_error)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(db_error)
    }

    pub fn stats(&self, now: i64) -> Result<Stats, String> {
        let (total_secs, sessions, games) = self
            .conn
            .query_row(
                "SELECT COALESCE(SUM(duration_secs), 0),
                        COUNT(*),
                        COUNT(DISTINCT appid)
                   FROM sessions
                  WHERE ended_at IS NOT NULL",
                [],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, i64>(2)?,
                    ))
                },
            )
            .map_err(db_error)?;
        let last_24h_secs: i64 = self
            .conn
            .query_row(
                "SELECT COALESCE(SUM(duration_secs), 0)
                   FROM sessions
                  WHERE ended_at IS NOT NULL AND started_at >= ?1",
                params![now - 86_400],
                |row| row.get(0),
            )
            .map_err(db_error)?;
        Ok(Stats {
            total_secs,
            sessions,
            games,
            last_24h_secs,
        })
    }

    pub fn per_game_totals(&self) -> Result<HashMap<u32, i64>, String> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT appid, COALESCE(SUM(duration_secs), 0)
                   FROM sessions
                  WHERE ended_at IS NOT NULL
                  GROUP BY appid",
            )
            .map_err(db_error)?;
        let rows = stmt
            .query_map([], |row| {
                Ok((row.get::<_, i64>(0)? as u32, row.get::<_, i64>(1)?))
            })
            .map_err(db_error)?;
        let mut totals = HashMap::new();
        for row in rows {
            let (appid, secs) = row.map_err(db_error)?;
            totals.insert(appid, secs);
        }
        Ok(totals)
    }

    // ---------------------------------------------------------------- queues

    pub fn queues(&self) -> Result<Vec<Queue>, String> {
        let queues: Vec<(i64, String)> = {
            let mut stmt = self
                .conn
                .prepare("SELECT id, name FROM queues ORDER BY name COLLATE NOCASE")
                .map_err(db_error)?;
            let rows = stmt
                .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
                .map_err(db_error)?;
            rows.collect::<Result<Vec<_>, _>>().map_err(db_error)?
        };

        let mut item_stmt = self
            .conn
            .prepare(
                "SELECT id, appid, game_name, minutes
                   FROM queue_items WHERE queue_id = ?1
                  ORDER BY position, id",
            )
            .map_err(db_error)?;

        let mut result = Vec::with_capacity(queues.len());
        for (id, name) in queues {
            let rows = item_stmt
                .query_map(params![id], |row| {
                    Ok(QueueItem {
                        id: row.get(0)?,
                        appid: row.get::<_, i64>(1)? as u32,
                        game_name: row.get(2)?,
                        minutes: row.get::<_, i64>(3)?.max(1) as u32,
                    })
                })
                .map_err(db_error)?;
            result.push(Queue {
                id,
                name,
                items: rows.collect::<Result<Vec<_>, _>>().map_err(db_error)?,
            });
        }
        Ok(result)
    }

    pub fn queue(&self, queue_id: i64) -> Result<Option<Queue>, String> {
        Ok(self.queues()?.into_iter().find(|queue| queue.id == queue_id))
    }

    pub fn create_queue(&self, name: &str) -> Result<i64, String> {
        let name = name.trim();
        if name.is_empty() {
            return Err("queue name cannot be empty".to_string());
        }
        self.conn
            .execute(
                "INSERT INTO queues (name, created_at) VALUES (?1, ?2)",
                params![name, unix_now()],
            )
            .map_err(|err| match err {
                rusqlite::Error::SqliteFailure(_, Some(message)) if message.contains("UNIQUE") => {
                    format!("a queue named '{name}' already exists")
                }
                other => format!("create queue: {other}"),
            })?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn delete_queue(&self, queue_id: i64) -> Result<(), String> {
        self.conn
            .execute("DELETE FROM queues WHERE id = ?1", params![queue_id])
            .map_err(db_error)?;
        Ok(())
    }

    pub fn add_queue_item(
        &self,
        queue_id: i64,
        appid: u32,
        game_name: &str,
        minutes: u32,
    ) -> Result<(), String> {
        let position: i64 = self
            .conn
            .query_row(
                "SELECT COALESCE(MAX(position), 0) + 1 FROM queue_items WHERE queue_id = ?1",
                params![queue_id],
                |row| row.get(0),
            )
            .map_err(db_error)?;
        self.conn
            .execute(
                "INSERT INTO queue_items (queue_id, position, appid, game_name, minutes)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    queue_id,
                    position,
                    appid,
                    game_name,
                    minutes.clamp(1, 24 * 60) as i64
                ],
            )
            .map_err(db_error)?;
        Ok(())
    }

    pub fn remove_queue_item(&self, item_id: i64) -> Result<(), String> {
        self.conn
            .execute("DELETE FROM queue_items WHERE id = ?1", params![item_id])
            .map_err(db_error)?;
        Ok(())
    }

    // -------------------------------------------------------------- settings

    pub fn load_settings(&self) -> Settings {
        let raw: Option<String> = self
            .conn
            .query_row(
                "SELECT value FROM settings WHERE key = ?1",
                params![SETTINGS_KEY],
                |row| row.get(0),
            )
            .optional()
            .ok()
            .flatten();
        raw.and_then(|json| serde_json::from_str::<Settings>(&json).ok())
            .unwrap_or_default()
            .sanitized()
    }

    pub fn save_settings(&self, settings: &Settings) -> Result<(), String> {
        let json = serde_json::to_string(settings)
            .map_err(|err| format!("serialize settings: {err}"))?;
        self.conn
            .execute(
                "INSERT INTO settings (key, value) VALUES (?1, ?2)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                params![SETTINGS_KEY, json],
            )
            .map_err(db_error)?;
        Ok(())
    }
}

fn session_from_row(row: &Row<'_>) -> rusqlite::Result<Session> {
    Ok(Session {
        id: row.get(0)?,
        appid: row.get::<_, i64>(1)? as u32,
        game_name: row.get(2)?,
        started_at: row.get(3)?,
        ended_at: row.get(4)?,
        duration_secs: row.get(5)?,
        source: SessionSource::parse(&row.get::<_, String>(6)?),
    })
}

fn path_string(path: &PathBuf) -> String {
    path.to_string_lossy().into_owned()
}

fn db_error(err: rusqlite::Error) -> String {
    format!("database error: {err}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    static COUNTER: AtomicU32 = AtomicU32::new(0);

    struct TempDb {
        dir: PathBuf,
        db: Db,
    }

    impl Drop for TempDb {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.dir);
        }
    }

    fn temp_db() -> TempDb {
        let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "steamidle-test-{}-{}-{unique}",
            std::process::id(),
            unix_now()
        ));
        fs::create_dir_all(&dir).unwrap();
        let db = Db::open(&dir.join("test.db")).unwrap();
        TempDb { dir, db }
    }

    #[test]
    fn sessions_roundtrip_and_stats() {
        let mut fixture = temp_db();
        let db = &mut fixture.db;
        let id = db
            .start_session(440, "Team Fortress 2", 1_000, SessionSource::Tracker)
            .unwrap();
        db.heartbeat(id, 1_060).unwrap();
        db.end_session(id, 1_120).unwrap();

        let sessions = db.recent_sessions(10).unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].duration_secs, 120);
        assert_eq!(sessions[0].ended_at, Some(1_120));

        let stats = db.stats(1_120).unwrap();
        assert_eq!(stats.total_secs, 120);
        assert_eq!(stats.sessions, 1);
        assert_eq!(stats.games, 1);

        let totals = db.per_game_totals().unwrap();
        assert_eq!(totals.get(&440), Some(&120));
    }

    #[test]
    fn open_sessions_recover_from_last_heartbeat() {
        let mut fixture = temp_db();
        let db = &mut fixture.db;
        let id = db
            .start_session(730, "Counter-Strike 2", 500, SessionSource::Tracker)
            .unwrap();
        db.heartbeat(id, 530).unwrap();
        // Simulate a crash: no end_session call.

        let recovered = db.recover_dangling_sessions().unwrap();
        assert_eq!(recovered, 1);

        let sessions = db.recent_sessions(5).unwrap();
        assert_eq!(sessions[0].ended_at, Some(530));
        assert_eq!(sessions[0].duration_secs, 30);
        assert!(db.recover_dangling_sessions().unwrap() == 0);
    }

    #[test]
    fn queues_and_settings_roundtrip() {
        let mut fixture = temp_db();
        let db = &mut fixture.db;
        let queue_id = db.create_queue("Evening test").unwrap();
        assert!(db.create_queue("Evening test").is_err());
        db.add_queue_item(queue_id, 440, "Team Fortress 2", 45)
            .unwrap();
        db.add_queue_item(queue_id, 730, "Counter-Strike 2", 0)
            .unwrap();

        let queues = db.queues().unwrap();
        assert_eq!(queues.len(), 1);
        assert_eq!(queues[0].items.len(), 2);
        assert_eq!(queues[0].items[0].minutes, 45);
        assert_eq!(queues[0].items[1].minutes, 1, "minutes are clamped");

        let item_id = queues[0].items[0].id;
        db.remove_queue_item(item_id).unwrap();
        assert_eq!(db.queues().unwrap()[0].items.len(), 1);

        let settings = Settings {
            start_with_windows: true,
            poll_seconds: 9,
            ..Settings::default()
        };
        db.save_settings(&settings).unwrap();
        assert_eq!(db.load_settings(), settings);

        db.delete_queue(queue_id).unwrap();
        assert!(db.queues().unwrap().is_empty());
    }

    #[test]
    fn games_upsert_is_idempotent() {
        let mut fixture = temp_db();
        let db = &mut fixture.db;
        let game = Game {
            appid: 440,
            name: "Team Fortress 2".into(),
            installed: true,
            size_on_disk: 42,
            ..Game::default()
        };
        db.upsert_games(&[game.clone()]).unwrap();
        db.upsert_games(&[game]).unwrap();
        let count: i64 = db
            .conn
            .query_row("SELECT COUNT(*) FROM games", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 1);
    }
}
