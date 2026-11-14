//! Dark, minimal dashboard. The window starts hidden (unless `--show` or the
//! tray is unavailable); closing it hides it again when the user prefers that.

use crate::engine::models::{format_duration, format_size, Game, Settings};
use crate::engine::unix_now;
use crate::engine::worker::{Command, Engine, Shared, Snapshot};
use crate::platform;
use crate::ui::theme;
use crate::ui::tray::{TrayAction, TrayHandle};
use egui::{Align, Color32, Layout, RichText, Stroke, Vec2};
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tab {
    Library,
    Sessions,
    Queues,
    Settings,
}

impl Tab {
    fn label(self) -> &'static str {
        match self {
            Self::Library => "Library",
            Self::Sessions => "Sessions",
            Self::Queues => "Queues",
            Self::Settings => "Settings",
        }
    }
}

pub struct App {
    engine: Engine,
    shared: Arc<Mutex<Shared>>,
    snap: Snapshot,
    tab: Tab,
    window_visible: bool,
    first_frame: bool,
    quitting: bool,
    tray: Option<TrayHandle>,
    tooltip: String,
    parked: bool,
    search: String,
    queue_selected: Option<i64>,
    queue_minutes: u32,
    new_queue_name: String,
    add_game_appid: u32,
    add_minutes: u32,
    settings: Settings,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, engine: Engine, show: bool) -> Self {
        theme::apply(&cc.egui_ctx);

        let shared = engine.shared();
        let snap = shared
            .lock()
            .map(|state| state.snapshot())
            .unwrap_or_default();
        let settings = snap.settings.clone();
        let queue_selected = snap.queues.first().map(|queue| queue.id);

        let tray = match TrayHandle::new(&cc.egui_ctx, Arc::clone(&shared)) {
            Ok(tray) => Some(tray),
            Err(err) => {
                crate::logging::warn(&err);
                None
            }
        };

        // Let tray events and later launches wake a hidden UI immediately.
        {
            let ctx = cc.egui_ctx.clone();
            if let Ok(mut state) = shared.lock() {
                state.waker = Some(Arc::new(move || ctx.request_repaint()));
            }
        }

        Self {
            engine,
            shared,
            snap,
            tab: Tab::Library,
            window_visible: show || tray.is_none(),
            first_frame: true,
            quitting: false,
            tray,
            tooltip: String::new(),
            parked: false,
            search: String::new(),
            queue_selected,
            queue_minutes: 30,
            new_queue_name: String::new(),
            add_game_appid: 0,
            add_minutes: 30,
            settings,
        }
    }

    fn refresh_snapshot(&mut self) {
        if let Ok(state) = self.shared.lock() {
            self.snap = state.snapshot();
        }
    }

    fn show_window(&mut self, ctx: &egui::Context) {
        self.window_visible = true;
        self.parked = false;
        // Center on the current monitor. Using the viewport's monitor size
        // avoids depending on a remembered position across hide/show cycles.
        let monitor = ctx
            .input(|input| input.viewport().monitor_size)
            .unwrap_or(egui::vec2(1920.0, 1080.0));
        let size = ctx.input(|input| input.viewport().inner_rect.map(|rect| rect.size()))
            .unwrap_or(egui::vec2(1060.0, 680.0));
        let position = egui::pos2(
            ((monitor.x - size.x) / 2.0).max(0.0),
            ((monitor.y - size.y) / 2.0).max(0.0),
        );
        ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(position));
        ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
        crate::logging::info(format!("dashboard shown at {position:?}"));
    }

    /// Park the dashboard off-screen. See `NativeOptions` in `main.rs` for why
    /// this is used instead of hiding the OS window.
    fn hide_window(&mut self, ctx: &egui::Context) {
        self.window_visible = false;
        self.park(ctx);
        crate::logging::info("dashboard hidden to tray");
    }

    /// One parking command only: each viewport command schedules a repaint, so
    /// repeating it would keep the hidden app painting.
    fn park(&mut self, ctx: &egui::Context) {
        self.parked = true;
        ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(egui::pos2(
            -32_000.0, -32_000.0,
        )));
    }

    fn quit(&mut self, ctx: &egui::Context) {
        if self.quitting {
            return;
        }
        self.quitting = true;
        self.engine.shutdown();
        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
    }

    fn handle_tray(&mut self, ctx: &egui::Context) {
        let actions = match self.tray.as_ref() {
            Some(tray) => tray.poll(),
            None => Vec::new(),
        };
        for action in actions {
            match action {
                TrayAction::Open => {
                    self.tab = Tab::Library;
                    self.show_window(ctx);
                }
                TrayAction::Settings => {
                    self.tab = Tab::Settings;
                    self.show_window(ctx);
                }
                TrayAction::Sessions => {
                    self.tab = Tab::Sessions;
                    self.show_window(ctx);
                }
                TrayAction::Exit => {
                    self.quit(ctx);
                    return;
                }
            }
        }
    }

    fn handle_close(&mut self, ctx: &egui::Context) {
        if self.quitting || !self.window_visible {
            return;
        }
        if ctx.input(|input| input.viewport().close_requested()) {
            if self.settings.close_to_tray {
                ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
                self.hide_window(ctx);
            } else {
                self.quit(ctx);
            }
        }
    }

    fn handle_show_request(&mut self, ctx: &egui::Context) {
        if !self.snap.show_requested {
            return;
        }
        if let Ok(mut state) = self.shared.lock() {
            state.show_requested = false;
        }
        self.snap.show_requested = false;
        self.show_window(ctx);
    }

    fn update_tooltip(&mut self) {
        let Some(tray) = self.tray.as_ref() else {
            return;
        };
        let tooltip = match self.snap.active.keys().next() {
            Some(appid) => {
                let name = self
                    .snap
                    .games
                    .iter()
                    .find(|game| game.appid == *appid)
                    .map(|game| game.name.as_str())
                    .unwrap_or("a game");
                format!("SteamIdle — tracking {name}")
            }
            None => "SteamIdle — idle; no game tracked".to_string(),
        };
        if tooltip != self.tooltip {
            tray.set_tooltip(&tooltip);
            self.tooltip = tooltip;
        }
    }

    fn save_settings(&mut self) {
        self.engine.send(Command::UpdateSettings(self.settings.clone()));
    }

    fn selected_queue_name(&self) -> String {
        self.queue_selected
            .and_then(|id| {
                self.snap
                    .queues
                    .iter()
                    .find(|queue| queue.id == id)
                    .map(|queue| queue.name.clone())
            })
            .unwrap_or_else(|| "Select queue".to_string())
    }

    // ------------------------------------------------------------------ header

    fn header(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label(
                RichText::new("SteamIdle")
                    .size(18.0)
                    .strong()
                    .color(theme::TEXT),
            );

            let (label, color) = if !self.snap.steam.installed {
                ("Steam not found", theme::BAD)
            } else if self.snap.steam.running {
                ("Steam running", theme::GOOD)
            } else {
                ("Steam closed", theme::WARN)
            };
            pill(ui, label, color);

            if let Some(appid) = self.snap.active.keys().next() {
                let name = self
                    .snap
                    .games
                    .iter()
                    .find(|game| game.appid == *appid)
                    .map(|game| game.name.as_str())
                    .unwrap_or("game");
                pill(ui, &format!("Tracking {name}"), theme::GOOD);
            }
            if let Some(run) = &self.snap.queue_run {
                pill(
                    ui,
                    &format!("Queue: {}", run.phase.label()),
                    theme::ACCENT,
                );
            }

            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui
                    .button("Refresh library")
                    .on_hover_text("Rescan Steam libraries and manifests")
                    .clicked()
                {
                    self.engine.send(Command::RefreshLibrary);
                }
                if ui.button("Start Steam").clicked() {
                    self.engine.send(Command::StartSteam);
                }
                let installed = self
                    .snap
                    .games
                    .iter()
                    .filter(|game| game.installed)
                    .count();
                ui.label(
                    RichText::new(format!("{installed} installed games"))
                        .size(11.0)
                        .color(theme::MUTED),
                );
            });
        });
    }

    fn nav(&mut self, ui: &mut egui::Ui) {
        ui.add_space(6.0);
        for tab in [Tab::Library, Tab::Sessions, Tab::Queues, Tab::Settings] {
            let selected = self.tab == tab;
            let text = RichText::new(tab.label())
                .size(13.5)
                .color(if selected { theme::TEXT } else { theme::MUTED });
            let button = egui::Button::new(text)
                .fill(if selected {
                    theme::ACCENT_DIM
                } else {
                    Color32::TRANSPARENT
                })
                .stroke(Stroke::NONE);
            if ui
                .add_sized(Vec2::new(ui.available_width(), 32.0), button)
                .clicked()
            {
                self.tab = tab;
            }
        }

        ui.with_layout(Layout::bottom_up(Align::LEFT), |ui| {
            ui.label(
                RichText::new(format!("v{} · MIT", env!("CARGO_PKG_VERSION")))
                    .size(10.0)
                    .color(theme::MUTED),
            );
            ui.label(
                RichText::new(if self.snap.steam.running {
                    "Steam client detected"
                } else {
                    "Steam client idle"
                })
                .size(10.5)
                .color(theme::MUTED),
            );
            if !self.snap.active.is_empty() {
                ui.label(
                    RichText::new(format!("{} game(s) running", self.snap.active.len()))
                        .size(10.5)
                        .color(theme::GOOD),
                );
            }
        });
    }

    fn status_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(format!("Database: {}", self.snap.data_path.display()))
                    .size(10.0)
                    .color(theme::MUTED),
            );
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.label(
                    RichText::new("Local only · no telemetry")
                        .size(10.0)
                        .color(theme::MUTED),
                );
            });
        });
    }

    // ----------------------------------------------------------------- library

    fn library_view(&mut self, ui: &mut egui::Ui) {
        let installed = self.snap.games.iter().filter(|game| game.installed).count();
        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut self.search)
                    .hint_text("Search games or AppID")
                    .desired_width(260.0),
            );
            ui.label(
                RichText::new(format!(
                    "{} installed · {} known",
                    installed,
                    self.snap.games.len()
                ))
                .size(11.5)
                .color(theme::MUTED),
            );

            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.label(
                    RichText::new(format!("{} min", self.queue_minutes))
                        .size(11.5)
                        .color(theme::MUTED),
                );
                ui.add(
                    egui::DragValue::new(&mut self.queue_minutes)
                        .range(1..=720)
                        .speed(1.0),
                );
                egui::ComboBox::from_id_salt("queue_picker")
                    .selected_text(self.selected_queue_name())
                    .width(160.0)
                    .show_ui(ui, |ui| {
                        for queue in &self.snap.queues {
                            ui.selectable_value(
                                &mut self.queue_selected,
                                Some(queue.id),
                                &queue.name,
                            );
                        }
                    });
                ui.label(
                    RichText::new("Add to queue")
                        .size(11.5)
                        .color(theme::MUTED),
                );
            });
        });
        ui.add_space(6.0);

        if !self.snap.steam.installed {
            hint(
                ui,
                "Steam was not found",
                "Install Steam, sign in once, then press Refresh library.",
            );
            return;
        }

        let needle = self.search.trim().to_lowercase();
        let games: Vec<Game> = self
            .snap
            .games
            .iter()
            .filter(|game| {
                needle.is_empty()
                    || game.name.to_lowercase().contains(&needle)
                    || game.appid.to_string().contains(&needle)
            })
            .cloned()
            .collect();

        if games.is_empty() {
            hint(
                ui,
                "No games match",
                "Try a different name, or press Refresh library.",
            );
            return;
        }

        let queue_selected = self.queue_selected;
        let queue_minutes = self.queue_minutes;
        let engine = &self.engine;
        let per_game = &self.snap.per_game;

        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                egui::Grid::new("games_grid")
                    .num_columns(6)
                    .spacing([16.0, 7.0])
                    .min_col_width(70.0)
                    .show(ui, |ui| {
                        table_header(ui, "GAME");
                        table_header(ui, "APPID");
                        table_header(ui, "STATUS");
                        table_header(ui, "SIZE");
                        table_header(ui, "TRACKED");
                        table_header(ui, "");
                        ui.end_row();

                        for game in &games {
                            let name = ui.label(RichText::new(&game.name).size(13.0));
                            if let Some(dir) = &game.install_dir {
                                name.on_hover_text(dir.display().to_string());
                            }
                            ui.label(
                                RichText::new(game.appid.to_string())
                                    .monospace()
                                    .size(11.5)
                                    .color(theme::MUTED),
                            );

                            let (status, color) = if game.running {
                                ("Running", theme::GOOD)
                            } else if game.installed {
                                ("Installed", theme::MUTED)
                            } else {
                                ("Missing", theme::BAD)
                            };
                            ui.label(RichText::new(status).size(11.5).color(color));
                            ui.label(
                                RichText::new(format_size(game.size_on_disk))
                                    .size(11.5)
                                    .color(theme::MUTED),
                            );
                            let tracked = per_game.get(&game.appid).copied().unwrap_or(0);
                            ui.label(
                                RichText::new(if tracked > 0 {
                                    format_duration(tracked)
                                } else {
                                    "—".to_string()
                                })
                                .size(11.5)
                                .color(theme::MUTED),
                            );

                            ui.horizontal(|ui| {
                                if ui
                                    .add_enabled(
                                        game.installed && !game.running,
                                        egui::Button::new("Launch").small(),
                                    )
                                    .on_hover_text("Launch through the installed Steam client")
                                    .clicked()
                                {
                                    engine.send(Command::LaunchGame(game.appid));
                                }
                                if ui
                                    .add_enabled(
                                        queue_selected.is_some(),
                                        egui::Button::new("Queue").small(),
                                    )
                                    .on_hover_text("Append to the selected launch queue")
                                    .clicked()
                                {
                                    if let Some(queue_id) = queue_selected {
                                        engine.send(Command::AddQueueItem {
                                            queue_id,
                                            appid: game.appid,
                                            game_name: game.name.clone(),
                                            minutes: queue_minutes,
                                        });
                                    }
                                }
                            });
                            ui.end_row();
                        }
                    });
            });
    }

    // ---------------------------------------------------------------- sessions

    fn sessions_view(&mut self, ui: &mut egui::Ui) {
        let now = unix_now();
        let stats = self.snap.stats.clone();

        ui.horizontal_wrapped(|ui| {
            stat_card(ui, "TOTAL TRACKED", &format_duration(stats.total_secs));
            stat_card(ui, "SESSIONS", &stats.sessions.to_string());
            stat_card(ui, "GAMES PLAYED", &stats.games.to_string());
            stat_card(ui, "LAST 24 HOURS", &format_duration(stats.last_24h_secs));
        });

        if !self.snap.active.is_empty() {
            ui.add_space(10.0);
            ui.horizontal_wrapped(|ui| {
                for (appid, started_at) in self.snap.active.clone() {
                    let name = self
                        .snap
                        .games
                        .iter()
                        .find(|game| game.appid == appid)
                        .map(|game| game.name.as_str())
                        .unwrap_or("game");
                    pill(
                        ui,
                        &format!(
                            "Now: {name} · {}",
                            format_duration(now - started_at)
                        ),
                        theme::GOOD,
                    );
                }
            });
        }

        ui.add_space(12.0);
        if self.snap.sessions.is_empty() {
            hint(
                ui,
                "No sessions tracked yet",
                "Start a game from Steam and its real runtime appears here.",
            );
            return;
        }

        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                egui::Grid::new("sessions_grid")
                    .num_columns(4)
                    .spacing([20.0, 7.0])
                    .min_col_width(90.0)
                    .striped(true)
                    .show(ui, |ui| {
                        table_header(ui, "GAME");
                        table_header(ui, "STARTED");
                        table_header(ui, "DURATION");
                        table_header(ui, "SOURCE");
                        ui.end_row();

                        for session in &self.snap.sessions {
                            ui.label(RichText::new(&session.game_name).size(13.0))
                                .on_hover_text(format!(
                                    "AppID {}\nSession #{}",
                                    session.appid, session.id
                                ));
                            ui.label(
                                RichText::new(format_timestamp(session.started_at))
                                    .size(11.5)
                                    .color(theme::MUTED),
                            );
                            ui.label(
                                RichText::new(format_duration(session.live_duration(now)))
                                    .size(12.0)
                                    .color(if session.ended_at.is_none() {
                                        theme::GOOD
                                    } else {
                                        theme::TEXT
                                    }),
                            );
                            ui.label(
                                RichText::new(session.source.as_str())
                                    .size(11.5)
                                    .color(theme::MUTED),
                            );
                            ui.end_row();
                        }
                    });
            });
    }

    // ------------------------------------------------------------------ queues

    fn queues_view(&mut self, ui: &mut egui::Ui) {
        let height = ui.available_height();
        ui.horizontal_top(|ui| {
            ui.allocate_ui(Vec2::new(250.0, height), |ui| self.queue_list(ui));
            ui.add_space(10.0);
            let width = ui.available_width();
            ui.allocate_ui(Vec2::new(width, height), |ui| self.queue_detail(ui));
        });
    }

    fn queue_list(&mut self, ui: &mut egui::Ui) {
        section_header(ui, "Launch queues", "Sequences of installed games with a dwell time. Steam performs every launch.");
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut self.new_queue_name)
                    .hint_text("New queue name")
                    .desired_width(150.0),
            );
            if ui.button("Create").clicked() && !self.new_queue_name.trim().is_empty() {
                self.engine
                    .send(Command::CreateQueue(self.new_queue_name.clone()));
                self.new_queue_name.clear();
            }
        });
        ui.add_space(8.0);

        let queues = self.snap.queues.clone();
        if queues.is_empty() {
            ui.label(
                RichText::new("No queues yet. Create one, then add games from the Library tab.")
                    .size(11.5)
                    .color(theme::MUTED),
            );
            return;
        }

        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                for queue in queues {
                    let selected = self.queue_selected == Some(queue.id);
                    ui.horizontal(|ui| {
                        let label = RichText::new(format!(
                            "{}  ({})",
                            queue.name,
                            queue.items.len()
                        ))
                        .size(12.5)
                        .color(if selected { theme::TEXT } else { theme::MUTED });
                        if ui
                            .add_sized(
                                Vec2::new(ui.available_width() - 26.0, 28.0),
                                egui::Button::new(label)
                                    .fill(if selected {
                                        theme::ACCENT_DIM
                                    } else {
                                        Color32::TRANSPARENT
                                    })
                                    .stroke(Stroke::NONE),
                            )
                            .clicked()
                        {
                            self.queue_selected = Some(queue.id);
                        }
                        if ui
                            .button("✕")
                            .on_hover_text("Delete this queue")
                            .clicked()
                        {
                            self.engine.send(Command::DeleteQueue(queue.id));
                            if self.queue_selected == Some(queue.id) {
                                self.queue_selected = None;
                            }
                        }
                    });
                }
            });
    }

    fn queue_detail(&mut self, ui: &mut egui::Ui) {
        let Some(queue_id) = self.queue_selected else {
            hint(
                ui,
                "Select a queue",
                "Queues launch your installed games one after another for testing.",
            );
            return;
        };
        let Some(queue) = self
            .snap
            .queues
            .iter()
            .find(|queue| queue.id == queue_id)
            .cloned()
        else {
            hint(ui, "Queue missing", "Pick another queue from the list.");
            return;
        };

        let run = self
            .snap
            .queue_run
            .as_ref()
            .filter(|run| run.name == queue.name)
            .cloned();

        ui.horizontal(|ui| {
            ui.label(
                RichText::new(&queue.name)
                    .size(16.0)
                    .strong()
                    .color(theme::TEXT),
            );
            if run.is_some() {
                if ui.button("Stop queue").clicked() {
                    self.engine.send(Command::StopQueue);
                }
            } else if ui
                .button("Run queue")
                .on_hover_text("Launch these games in order through Steam")
                .clicked()
            {
                self.engine.send(Command::StartQueue(queue.id));
            }
        });

        if let Some(run) = &run {
            egui::Frame::NONE
                .fill(theme::CARD)
                .stroke(Stroke::new(1.0, theme::BORDER))
                .corner_radius(egui::CornerRadius::same(10))
                .inner_margin(egui::Margin::symmetric(12, 10))
                .show(ui, |ui| {
                    ui.label(
                        RichText::new(format!(
                            "Slot {} of {} · {}",
                            (run.index + 1).min(run.total),
                            run.total,
                            run.game_name
                        ))
                        .size(12.5)
                        .color(theme::TEXT),
                    );
                    ui.label(
                        RichText::new(&run.message)
                            .size(11.5)
                            .color(theme::MUTED),
                    );
                    if run.remaining_secs > 0 {
                        ui.label(
                            RichText::new(format!(
                                "{} remaining",
                                format_duration(run.remaining_secs)
                            ))
                            .size(11.5)
                            .color(theme::ACCENT),
                        );
                    }
                });
            ui.add_space(8.0);
        }

        ui.add_space(6.0);
        if queue.items.is_empty() {
            ui.label(
                RichText::new("This queue is empty. Add games from your library below.")
                    .size(11.5)
                    .color(theme::MUTED),
            );
        } else {
            egui::Grid::new("queue_items_grid")
                .num_columns(4)
                .spacing([14.0, 6.0])
                .min_col_width(70.0)
                .show(ui, |ui| {
                    table_header(ui, "");
                    table_header(ui, "GAME");
                    table_header(ui, "DWELL");
                    table_header(ui, "");
                    ui.end_row();
                    for (index, item) in queue.items.iter().enumerate() {
                        ui.label(
                            RichText::new(format!("{}", index + 1))
                                .size(11.5)
                                .color(theme::MUTED),
                        );
                        ui.label(RichText::new(&item.game_name).size(12.5));
                        ui.label(
                            RichText::new(format!("{} min", item.minutes))
                                .size(11.5)
                                .color(theme::MUTED),
                        );
                        if ui.small_button("Remove").clicked() {
                            self.engine.send(Command::RemoveQueueItem(item.id));
                        }
                        ui.end_row();
                    }
                });
        }

        ui.add_space(12.0);
        section_header(ui, "Add a game", "Only installed Steam games are queued and launched.");
        ui.horizontal(|ui| {
            let installed: Vec<Game> = self
                .snap
                .games
                .iter()
                .filter(|game| game.installed)
                .cloned()
                .collect();
            let selected_name = installed
                .iter()
                .find(|game| game.appid == self.add_game_appid)
                .map(|game| game.name.clone())
                .unwrap_or_else(|| "Select a game".to_string());
            egui::ComboBox::from_id_salt("queue_add_game")
                .selected_text(selected_name)
                .width(240.0)
                .show_ui(ui, |ui| {
                    for game in &installed {
                        ui.selectable_value(&mut self.add_game_appid, game.appid, &game.name);
                    }
                });
            ui.add(egui::DragValue::new(&mut self.add_minutes).range(1..=720));
            ui.label(
                RichText::new("minutes")
                    .size(11.5)
                    .color(theme::MUTED),
            );
            let can_add = installed
                .iter()
                .any(|game| game.appid == self.add_game_appid);
            if ui.add_enabled(can_add, egui::Button::new("Add")).clicked() {
                if let Some(game) = installed
                    .iter()
                    .find(|game| game.appid == self.add_game_appid)
                {
                    self.engine.send(Command::AddQueueItem {
                        queue_id: queue.id,
                        appid: game.appid,
                        game_name: game.name.clone(),
                        minutes: self.add_minutes,
                    });
                }
            }
        });

        ui.add_space(16.0);
        section_header(ui, "Recent activity", "Local log of tracker and queue events.");
        ui.add_space(6.0);
        egui::ScrollArea::vertical()
            .max_height(ui.available_height().max(80.0))
            .auto_shrink([false, false])
            .show(ui, |ui| {
                if self.snap.activity.is_empty() {
                    ui.label(
                        RichText::new("Nothing yet.")
                            .size(11.5)
                            .color(theme::MUTED),
                    );
                }
                for line in &self.snap.activity {
                    ui.horizontal(|ui| {
                        ui.label(
                            RichText::new(format_timestamp(line.at))
                                .monospace()
                                .size(10.0)
                                .color(theme::MUTED),
                        );
                        ui.label(RichText::new(&line.text).size(11.5).color(theme::TEXT));
                    });
                }
            });
    }

    // ---------------------------------------------------------------- settings

    fn settings_view(&mut self, ui: &mut egui::Ui) {
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                section_header(ui, "Startup", "SteamIdle starts in the system tray, not as a visible window.");

                let mut start_with_windows = self.settings.start_with_windows;
                if ui
                    .checkbox(&mut start_with_windows, "Start SteamIdle with Windows")
                    .changed()
                {
                    self.settings.start_with_windows = start_with_windows;
                    self.save_settings();
                }
                ui.label(
                    RichText::new("Off by default. Writes a single per-user startup entry; no admin rights needed.")
                        .size(11.0)
                        .color(theme::MUTED),
                );

                ui.add_space(14.0);
                section_header(ui, "Behaviour", "Sampling running processes is the only periodic work.");

                let mut close_to_tray = self.settings.close_to_tray;
                if ui
                    .checkbox(&mut close_to_tray, "Closing the window hides it to the tray")
                    .changed()
                {
                    self.settings.close_to_tray = close_to_tray;
                    self.save_settings();
                }

                let mut poll_seconds = self.settings.poll_seconds;
                ui.horizontal(|ui| {
                    ui.label("Check running games every");
                    if ui
                        .add(egui::Slider::new(&mut poll_seconds, 2..=30).suffix(" s"))
                        .changed()
                    {
                        self.settings.poll_seconds = poll_seconds;
                        self.save_settings();
                    }
                });

                ui.add_space(14.0);
                section_header(ui, "Steam", "Read-only discovery of the installed client and libraries.");
                ui.label(
                    RichText::new(match &self.snap.steam.root {
                        Some(root) => format!("Steam: {}", root.display()),
                        None => "Steam installation: not found".to_string(),
                    })
                    .size(11.5)
                    .color(if self.snap.steam.installed {
                        theme::TEXT
                    } else {
                        theme::WARN
                    }),
                );
                ui.label(
                    RichText::new(format!(
                        "{} library folder(s) detected",
                        self.snap.steam.libraries.len()
                    ))
                    .size(11.5)
                    .color(theme::MUTED),
                );
                for library in &self.snap.steam.libraries {
                    ui.label(
                        RichText::new(library.display().to_string())
                            .size(10.5)
                            .color(theme::MUTED),
                    );
                }
                if let Some(exe) = &self.snap.steam.steam_exe {
                    ui.label(
                        RichText::new(format!("Client executable: {}", exe.display()))
                            .size(10.5)
                            .color(theme::MUTED),
                    );
                }
                ui.horizontal(|ui| {
                    if ui.button("Refresh library").clicked() {
                        self.engine.send(Command::RefreshLibrary);
                    }
                    if ui.button("Start Steam").clicked() {
                        self.engine.send(Command::StartSteam);
                    }
                });

                ui.add_space(14.0);
                section_header(ui, "Data", "One portable SQLite database, stored next to the executable.");
                ui.label(
                    RichText::new(self.snap.data_path.display().to_string())
                        .size(11.5)
                        .color(theme::TEXT),
                );
                ui.horizontal(|ui| {
                    if ui.button("Open data folder").clicked() {
                        if let Err(err) = platform::open_path(&self.snap.data_path) {
                            crate::logging::warn(&err);
                        }
                    }
                    if ui.button("Refresh library").clicked() {
                        self.engine.send(Command::RefreshLibrary);
                    }
                });
                ui.label(
                    RichText::new(format!(
                        "{} sessions stored · {} tracked games",
                        self.snap.stats.sessions, self.snap.stats.games
                    ))
                    .size(11.0)
                    .color(theme::MUTED),
                );

                ui.add_space(14.0);
                section_header(ui, "About SteamIdle", "An independent, open-source tray companion.");
                ui.label(
                    RichText::new(
                        "SteamIdle never asks for Steam credentials, never impersonates the Steam client, never alters playtime, and sends no telemetry. Games are launched through Steam itself, so ownership checks and anti-cheat stay intact. Sessions are derived only from game processes that are actually running.",
                    )
                    .size(11.5)
                    .color(theme::MUTED),
                );
                ui.add_space(4.0);
                ui.label(
                    RichText::new(format!(
                        "Version {} · MIT License · built with Rust and egui",
                        env!("CARGO_PKG_VERSION")
                    ))
                    .size(10.5)
                    .color(theme::MUTED),
                );
            });
    }
}

impl eframe::App for App {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if self.first_frame {
            self.first_frame = false;
            if self.window_visible {
                self.show_window(ctx);
            } else {
                self.park(ctx);
            }
        }
        self.handle_tray(ctx);
        self.handle_close(ctx);
        self.refresh_snapshot();
        self.handle_show_request(ctx);
        self.update_tooltip();
        if !self.window_visible && !self.parked {
            self.park(ctx);
        }
        // While hidden, eframe would repaint a hidden window every 100 ms once
        // a repaint is scheduled, so we schedule nothing at all. Tray clicks
        // and later launches wake the UI explicitly through `Shared::waker`.
        if self.window_visible {
            ctx.request_repaint_after(Duration::from_millis(500));
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let header_frame = egui::Frame::NONE
            .fill(theme::BG)
            .inner_margin(egui::Margin::symmetric(14, 8));
        egui::Panel::top("header")
            .resizable(false)
            .show_separator_line(false)
            .frame(header_frame)
            .show(ui, |ui| self.header(ui));

        let status_frame = egui::Frame::NONE
            .fill(theme::BG)
            .inner_margin(egui::Margin::symmetric(14, 5));
        egui::Panel::bottom("status")
            .resizable(false)
            .show_separator_line(false)
            .frame(status_frame)
            .show(ui, |ui| self.status_bar(ui));

        let nav_frame = egui::Frame::NONE
            .fill(theme::PANEL)
            .inner_margin(egui::Margin::symmetric(10, 10));
        egui::Panel::left("nav")
            .resizable(false)
            .default_size(190.0)
            .frame(nav_frame)
            .show(ui, |ui| self.nav(ui));

        let central_frame = egui::Frame::NONE
            .fill(theme::BG)
            .inner_margin(egui::Margin::same(14));
        egui::CentralPanel::default()
            .frame(central_frame)
            .show(ui, |ui| match self.tab {
                Tab::Library => self.library_view(ui),
                Tab::Sessions => self.sessions_view(ui),
                Tab::Queues => self.queues_view(ui),
                Tab::Settings => self.settings_view(ui),
            });
    }

    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        theme::BG.to_normalized_gamma_f32()
    }
}

// ------------------------------------------------------------------- helpers

fn pill(ui: &mut egui::Ui, text: &str, color: Color32) {
    egui::Frame::NONE
        .fill(color.gamma_multiply(0.14))
        .stroke(Stroke::new(1.0, color.gamma_multiply(0.45)))
        .corner_radius(egui::CornerRadius::same(9))
        .inner_margin(egui::Margin::symmetric(7, 2))
        .show(ui, |ui| {
            ui.label(RichText::new(text).size(10.5).color(color));
        });
}

fn stat_card(ui: &mut egui::Ui, label: &str, value: &str) {
    egui::Frame::NONE
        .fill(theme::CARD)
        .stroke(Stroke::new(1.0, theme::BORDER))
        .corner_radius(egui::CornerRadius::same(10))
        .inner_margin(egui::Margin::symmetric(14, 9))
        .show(ui, |ui| {
            ui.vertical(|ui| {
                ui.label(
                    RichText::new(label)
                        .size(9.5)
                        .color(theme::MUTED),
                );
                ui.label(RichText::new(value).size(18.0).strong().color(theme::TEXT));
            });
        });
}

fn section_header(ui: &mut egui::Ui, title: &str, subtitle: &str) {
    ui.label(RichText::new(title).size(14.0).strong().color(theme::TEXT));
    ui.label(RichText::new(subtitle).size(11.0).color(theme::MUTED));
    ui.add_space(4.0);
}

fn table_header(ui: &mut egui::Ui, text: &str) {
    ui.label(RichText::new(text).size(9.5).strong().color(theme::MUTED));
}

fn hint(ui: &mut egui::Ui, title: &str, body: &str) {
    ui.add_space(30.0);
    ui.vertical_centered(|ui| {
        ui.label(RichText::new(title).size(14.0).color(theme::TEXT));
        ui.label(RichText::new(body).size(11.5).color(theme::MUTED));
    });
}

fn format_timestamp(timestamp: i64) -> String {
    use chrono::{Local, TimeZone};
    Local
        .timestamp_opt(timestamp, 0)
        .single()
        .map(|time| time.format("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_else(|| "—".to_string())
}

