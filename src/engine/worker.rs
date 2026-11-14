//! The background worker thread.
//!
//! It owns the database connection and is the only writer. The dashboard reads
//! a cloned [`Snapshot`] and sends [`Command`]s, so no locks are held while the
//! UI draws and the engine never blocks on rendering.
//!
//! Session tracking is purely observational: a session starts when a game
//! process owned by an installed Steam library appears and ends when it
//! disappears. Heartbeats are written every poll so a crash loses at most one
//! poll interval of observed playtime.

use crate::engine::db::Db;
use crate::engine::models::*;
use crate::engine::queue::{Action, QueueRunView, QueueRunner};
use crate::engine::{autostart, steam, unix_now};
use crate::logging;
use crate::platform::{self, ProcInfo};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

/// How often the installed-game list is refreshed in the background.
const LIBRARY_REFRESH_SECS: i64 = 60;
/// Recent sessions kept in memory for the dashboard.
const SESSION_PAGE: u32 = 200;
const ACTIVITY_LIMIT: usize = 150;

#[derive(Debug, Clone)]
pub struct ActivityLine {
    pub at: i64,
    pub text: String,
}

/// Shared, read-only-to-the-UI engine state.
pub struct Shared {
    pub games: Vec<Game>,
    pub sessions: Vec<Session>,
    pub stats: Stats,
    pub per_game: HashMap<u32, i64>,
    pub queues: Vec<Queue>,
    pub steam: SteamStatus,
    pub queue_run: Option<QueueRunView>,
    pub activity: Vec<ActivityLine>,
    pub data_path: PathBuf,
    pub settings: Settings,
    /// AppID -> start timestamp of the session being tracked right now.
    pub active: HashMap<u32, i64>,
    /// Set when another launch asks this instance to show its dashboard.
    pub show_requested: bool,
    /// UI wake-up hook, installed by the dashboard (used by tray and
    /// second-instance events instead of polling).
    pub waker: Option<Arc<dyn Fn() + Send + Sync>>,
    /// Commands queued from the tray while the window is hidden, so they can
    /// run without waking (and repainting) the dashboard.
    pub pending: Vec<Command>,
}

impl Shared {
    fn new(settings: Settings, data_path: PathBuf) -> Self {
        Self {
            games: Vec::new(),
            sessions: Vec::new(),
            stats: Stats::default(),
            per_game: HashMap::new(),
            queues: Vec::new(),
            steam: SteamStatus::default(),
            queue_run: None,
            activity: Vec::new(),
            data_path,
            settings,
            active: HashMap::new(),
            show_requested: false,
            waker: None,
            pending: Vec::new(),
        }
    }

    pub fn log(&mut self, text: impl Into<String>) {
        self.activity.insert(
            0,
            ActivityLine {
                at: unix_now(),
                text: text.into(),
            },
        );
        self.activity.truncate(ACTIVITY_LIMIT);
    }

    pub fn snapshot(&self) -> Snapshot {
        Snapshot {
            games: self.games.clone(),
            sessions: self.sessions.clone(),
            stats: self.stats.clone(),
            per_game: self.per_game.clone(),
            queues: self.queues.clone(),
            steam: self.steam.clone(),
            queue_run: self.queue_run.clone(),
            activity: self.activity.clone(),
            data_path: self.data_path.clone(),
            settings: self.settings.clone(),
            active: self.active.clone(),
            show_requested: self.show_requested,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct Snapshot {
    pub games: Vec<Game>,
    pub sessions: Vec<Session>,
    pub stats: Stats,
    pub per_game: HashMap<u32, i64>,
    pub queues: Vec<Queue>,
    pub steam: SteamStatus,
    pub queue_run: Option<QueueRunView>,
    pub activity: Vec<ActivityLine>,
    pub data_path: PathBuf,
    pub settings: Settings,
    pub active: HashMap<u32, i64>,
    /// Set when another launch asks this instance to show its dashboard.
    pub show_requested: bool,
}

#[derive(Debug, Clone)]
pub enum Command {
    RefreshLibrary,
    LaunchGame(u32),
    StartSteam,
    UpdateSettings(Settings),
    StartQueue(i64),
    StopQueue,
    CreateQueue(String),
    DeleteQueue(i64),
    AddQueueItem {
        queue_id: i64,
        appid: u32,
        game_name: String,
        minutes: u32,
    },
    RemoveQueueItem(i64),
    Shutdown,
}

pub struct Engine {
    shared: Arc<Mutex<Shared>>,
    tx: Sender<Command>,
    join: Option<JoinHandle<()>>,
}

impl Engine {
    pub fn start(db: Db, settings: Settings) -> Self {
        let data_path = db.path().to_path_buf();
        let shared = Arc::new(Mutex::new(Shared::new(settings.clone(), data_path)));
        let (tx, rx) = channel();

        let worker_shared = Arc::clone(&shared);
        let join = std::thread::Builder::new()
            .name("steamidle-worker".to_string())
            .spawn(move || {
                Worker {
                    db,
                    shared: worker_shared,
                    rx,
                    games: Vec::new(),
                    active: HashMap::new(),
                    queue: None,
                    last_scan: 0,
                    last_poll: 0,
                    settings,
                    steam: SteamStatus::default(),
                    running: HashMap::new(),
                    process_cache: HashMap::new(),
                }
                .run();
            })
            .ok();

        Self { shared, tx, join }
    }

    pub fn shared(&self) -> Arc<Mutex<Shared>> {
        Arc::clone(&self.shared)
    }

    pub fn send(&self, command: Command) {
        let _ = self.tx.send(command);
    }

    /// Stop the worker, close open sessions and flush the database.
    pub fn shutdown(&mut self) {
        let _ = self.tx.send(Command::Shutdown);
        if let Some(handle) = self.join.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        let _ = self.tx.send(Command::Shutdown);
        if let Some(handle) = self.join.take() {
            let _ = handle.join();
        }
    }
}

struct ActiveSession {
    db_id: i64,
    started_at: i64,
}

struct Worker {
    db: Db,
    shared: Arc<Mutex<Shared>>,
    rx: Receiver<Command>,
    games: Vec<Game>,
    active: HashMap<u32, ActiveSession>,
    queue: Option<QueueRunner>,
    last_scan: i64,
    last_poll: i64,
    settings: Settings,
    steam: SteamStatus,
    running: HashMap<u32, Vec<u32>>,
    /// (pid, name) -> executable path, so repeated polls only inspect new
    /// processes instead of opening every process handle again.
    process_cache: HashMap<(u32, String), Option<PathBuf>>,
}

impl Worker {
    fn run(mut self) {
        self.scan(true);
        self.poll_processes();

        loop {
            match self.rx.recv_timeout(Duration::from_millis(300)) {
                Ok(Command::Shutdown) => break,
                Ok(command) => self.handle(command),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => break,
            }

            let pending = self
                .shared
                .lock()
                .map(|mut state| std::mem::take(&mut state.pending))
                .unwrap_or_default();
            for command in pending {
                self.handle(command);
            }

            let now = unix_now();
            if now - self.last_scan >= LIBRARY_REFRESH_SECS {
                self.scan(false);
            }
            if now - self.last_poll >= self.settings.poll_seconds as i64 {
                self.poll_processes();
            }
            self.tick_queue();
        }

        self.finish();
    }

    fn finish(&mut self) {
        let now = unix_now();
        for active in std::mem::take(&mut self.active).into_values() {
            let _ = self.db.end_session(active.db_id, now);
        }
        self.db.flush();
        if let Ok(mut shared) = self.shared.lock() {
            shared.active.clear();
            shared.queue_run = None;
        }
        logging::info("worker stopped; open sessions closed");
    }

    fn handle(&mut self, command: Command) {
        match command {
            Command::Shutdown => {}
            Command::RefreshLibrary => {
                self.scan(true);
                self.last_poll = 0;
                self.poll_processes();
                self.refresh_queues();
            }
            Command::LaunchGame(appid) => match self.games.iter().find(|game| game.appid == appid) {
                Some(game) if game.installed => match steam::launch_game(appid) {
                    Ok(()) => self.log(format!("Asked Steam to launch {}", game.name)),
                    Err(err) => self.log(format!("Could not launch {}: {err}", game.name)),
                },
                Some(game) => self.log(format!("{} is not fully installed", game.name)),
                None => self.log(format!("Unknown AppID {appid}")),
            },
            Command::StartSteam => match steam::launch_steam(&self.steam) {
                Ok(()) => self.log("Starting the Steam client"),
                Err(err) => self.log(format!("Could not start Steam: {err}")),
            },
            Command::UpdateSettings(settings) => {
                let settings = settings.sanitized();
                if settings.start_with_windows != autostart::is_enabled() {
                    match autostart::set_enabled(settings.start_with_windows) {
                        Ok(()) => self.log(if settings.start_with_windows {
                            "Start with Windows enabled".to_string()
                        } else {
                            "Start with Windows disabled".to_string()
                        }),
                        Err(err) => self.log(format!("Startup change failed: {err}")),
                    }
                }
                if let Err(err) = self.db.save_settings(&settings) {
                    logging::error(&err);
                }
                self.settings = settings.clone();
                self.last_poll = 0;
                if let Ok(mut shared) = self.shared.lock() {
                    shared.settings = settings;
                }
            }
            Command::StartQueue(queue_id) => self.start_queue(queue_id),
            Command::StopQueue => self.stop_queue("Queue stopped"),
            Command::CreateQueue(name) => match self.db.create_queue(&name) {
                Ok(_) => {
                    self.log(format!("Queue \"{}\" created", name.trim()));
                    self.refresh_queues();
                }
                Err(err) => self.log(err),
            },
            Command::DeleteQueue(queue_id) => {
                if self
                    .queue
                    .as_ref()
                    .is_some_and(|runner| runner.queue_id == queue_id)
                {
                    self.stop_queue("Queue deleted");
                }
                if let Err(err) = self.db.delete_queue(queue_id) {
                    self.log(err);
                }
                self.refresh_queues();
            }
            Command::AddQueueItem {
                queue_id,
                appid,
                game_name,
                minutes,
            } => {
                match self.db.add_queue_item(queue_id, appid, &game_name, minutes) {
                    Ok(()) => self.log(format!("Queued {game_name} for {minutes} min")),
                    Err(err) => self.log(err),
                }
                self.refresh_queues();
            }
            Command::RemoveQueueItem(item_id) => {
                if let Err(err) = self.db.remove_queue_item(item_id) {
                    self.log(err);
                }
                self.refresh_queues();
            }
        }
    }

    fn scan(&mut self, announce: bool) {
        let result = steam::scan();
        let now = unix_now();

        if let Err(err) = self.db.upsert_games(&result.games) {
            logging::error(&err);
        }
        self.games = result.games;
        self.steam = result.status;
        self.steam.running = steam::is_steam_running(&platform::list_processes());
        let queues = self.db.queues().unwrap_or_default();

        let installed = self.games.iter().filter(|game| game.installed).count();
        let library_count = self.steam.libraries.len();

        if let Ok(mut shared) = self.shared.lock() {
            shared.games = self.games.clone();
            shared.steam = self.steam.clone();
            shared.queues = queues;
            if announce {
                let message = if !shared.steam.installed {
                    "Steam installation not found".to_string()
                } else {
                    format!("Found {installed} installed games in {library_count} libraries")
                };
                shared.log(message.clone());
                logging::info(message);
            }
            for game in &mut shared.games {
                game.running = self.running.contains_key(&game.appid);
            }
        }
        self.last_scan = now;
    }

    /// Process list with executable paths, reusing paths from earlier polls.
    fn process_snapshot(&mut self) -> Vec<ProcInfo> {
        let base = platform::list_processes();
        let mut seen = HashSet::with_capacity(base.len());
        let mut processes = Vec::with_capacity(base.len());
        for process in base {
            let key = (process.pid, process.name.clone());
            seen.insert(key.clone());
            let exe = match self.process_cache.get(&key) {
                Some(known) => known.clone(),
                None => {
                    let path = platform::process_image_path(process.pid);
                    self.process_cache.insert(key, path.clone());
                    path
                }
            };
            processes.push(ProcInfo {
                pid: process.pid,
                name: process.name,
                exe,
            });
        }
        self.process_cache.retain(|key, _| seen.contains(key));
        processes
    }

    fn poll_processes(&mut self) {
        let started = unix_now();
        let processes = self.process_snapshot();
        let running = steam::running_map(&self.games, &processes);
        let steam_running = steam::is_steam_running(&processes);

        let mut started_names = Vec::new();
        let mut ended_names = Vec::new();

        for appid in running.keys() {
            if self.active.contains_key(appid) {
                if let Some(active) = self.active.get(appid) {
                    let _ = self.db.heartbeat(active.db_id, started);
                }
                continue;
            }
            let Some(game) = self.games.iter().find(|game| game.appid == *appid) else {
                continue;
            };
            let source = if self
                .queue
                .as_ref()
                .is_some_and(|runner| runner.current_appid() == Some(*appid))
            {
                SessionSource::Queue
            } else {
                SessionSource::Tracker
            };
            match self
                .db
                .start_session(*appid, &game.name, started, source)
            {
                Ok(db_id) => {
                    self.active.insert(
                        *appid,
                        ActiveSession {
                            db_id,
                            started_at: started,
                        },
                    );
                    started_names.push(game.name.clone());
                }
                Err(err) => logging::error(&err),
            }
        }

        let finished: Vec<u32> = self
            .active
            .keys()
            .filter(|appid| !running.contains_key(*appid))
            .copied()
            .collect();
        for appid in finished {
            if let Some(active) = self.active.remove(&appid) {
                let name = self
                    .games
                    .iter()
                    .find(|game| game.appid == appid)
                    .map(|game| game.name.clone())
                    .unwrap_or_else(|| format!("AppID {appid}"));
                let _ = self.db.end_session(active.db_id, started);
                let seconds = (started - active.started_at).max(0);
                ended_names.push(format!(
                    "Session ended: {name} ({})",
                    format_duration(seconds)
                ));
            }
        }

        self.running = running;
        self.steam.running = steam_running;
        self.last_poll = started;

        let sessions = self.db.recent_sessions(SESSION_PAGE).unwrap_or_default();
        let stats = self.db.stats(started).unwrap_or_default();
        let per_game = self.db.per_game_totals().unwrap_or_default();

        if let Ok(mut shared) = self.shared.lock() {
            shared.active = self
                .active
                .iter()
                .map(|(appid, active)| (*appid, active.started_at))
                .collect();
            for game in &mut shared.games {
                game.running = self.running.contains_key(&game.appid);
            }
            shared.steam.running = steam_running;
            for name in started_names {
                shared.log(format!("Session started: {name}"));
            }
            for line in ended_names {
                shared.log(line);
            }
            shared.sessions = sessions;
            shared.stats = stats;
            shared.per_game = per_game;
        }
    }

    fn tick_queue(&mut self) {
        let Some(mut runner) = self.queue.take() else {
            return;
        };
        let now = unix_now();
        let actions = runner.tick(now, &self.running);

        for action in actions {
            match action {
                Action::Launch { appid } => {
                    let name = runner
                        .items
                        .get(runner.index)
                        .map(|item| item.game_name.clone())
                        .unwrap_or_else(|| format!("AppID {appid}"));
                    match steam::launch_game(appid) {
                        Ok(()) => self.log(format!("Queue: launching {name}")),
                        Err(err) => self.log(format!("Queue: could not launch {name}: {err}")),
                    }
                }
                Action::Close { pids } => {
                    platform::request_close(&pids);
                    self.log("Queue: asked the game to close");
                }
                Action::Kill { pids } => {
                    for pid in &pids {
                        platform::terminate(*pid);
                    }
                    self.log("Queue: ended the game process");
                }
            }
        }

        let finished = runner.is_finished();
        let view = runner.snapshot(now);
        if finished {
            self.log(format!("Queue \"{}\" complete", runner.name));
        } else {
            self.queue = Some(runner);
        }

        if let Ok(mut shared) = self.shared.lock() {
            shared.queue_run = if finished { None } else { Some(view) };
        }
    }

    fn start_queue(&mut self, queue_id: i64) {
        let queue = match self.db.queue(queue_id) {
            Ok(Some(queue)) => queue,
            Ok(None) => {
                self.log("That queue no longer exists");
                return;
            }
            Err(err) => {
                self.log(err);
                return;
            }
        };
        if queue.items.is_empty() {
            self.log(format!("Queue \"{}\" has no games yet", queue.name));
            return;
        }
        if self.queue.is_some() {
            self.stop_queue("Queue replaced");
        }

        let installed: Vec<QueueItem> = queue
            .items
            .iter()
            .filter(|item| {
                self.games
                    .iter()
                    .any(|game| game.appid == item.appid && game.installed)
            })
            .cloned()
            .collect();
        if installed.is_empty() {
            self.log(format!(
                "None of the games in \"{}\" are installed right now",
                queue.name
            ));
            return;
        }
        if installed.len() != queue.items.len() {
            self.log(format!(
                "Skipping {} uninstalled game(s) in \"{}\"",
                queue.items.len() - installed.len(),
                queue.name
            ));
        }

        let Some(runner) = QueueRunner::new(queue.id, queue.name.clone(), installed, unix_now()) else {
            return;
        };
        self.log(format!(
            "Queue \"{}\" started ({} games)",
            queue.name,
            runner.items.len()
        ));
        if let Ok(mut shared) = self.shared.lock() {
            shared.queue_run = Some(runner.snapshot(unix_now()));
        }
        self.queue = Some(runner);
    }

    fn stop_queue(&mut self, reason: &str) {
        if self.queue.take().is_some() {
            self.log(reason.to_string());
        }
        if let Ok(mut shared) = self.shared.lock() {
            shared.queue_run = None;
        }
    }

    fn refresh_queues(&mut self) {
        match self.db.queues() {
            Ok(queues) => {
                if let Ok(mut shared) = self.shared.lock() {
                    shared.queues = queues;
                }
            }
            Err(err) => logging::error(&err),
        }
    }

    fn log(&mut self, message: impl Into<String>) {
        let message = message.into();
        logging::info(&message);
        if let Ok(mut shared) = self.shared.lock() {
            shared.log(message);
        }
    }
}
