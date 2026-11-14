// No console window: this is a tray application.
#![cfg_attr(windows, windows_subsystem = "windows")]

mod app;
mod engine;
mod icon;
mod logging;
mod paths;
mod platform;
mod ui;

use crate::engine::db::Db;
use crate::engine::worker::Engine;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let show = args.iter().any(|arg| arg == "--show");
    let _ = args.iter().any(|arg| arg == "--tray");

    // Only one tracker may write sessions; a second launch just surfaces the
    // existing dashboard.
    let Some(_instance) = platform::single_instance() else {
        platform::request_show_existing();
        return;
    };

    if let Err(err) = run(show) {
        logging::error(&err);
        platform::fatal_error(&format!("SteamIdle could not start.\n\n{err}"));
    }
}

fn run(show: bool) -> Result<(), String> {
    let data_dir = paths::data_dir();
    std::fs::create_dir_all(&data_dir)
        .map_err(|err| format!("create {}: {err}", data_dir.display()))?;
    logging::init(paths::log_file());

    let mut db = Db::open(&paths::database_file())?;
    match db.recover_dangling_sessions() {
        Ok(0) => {}
        Ok(count) => logging::info(format!(
            "closed {count} session(s) left open by the previous run"
        )),
        Err(err) => logging::warn(err),
    }

    let mut settings = db.load_settings();
    settings.start_with_windows = engine::autostart::is_enabled();
    let _ = db.save_settings(&settings);

    let engine = Engine::start(db, settings);
    {
        let shared = engine.shared();
        platform::spawn_show_listener(move || {
            let waker = {
                let Ok(mut state) = shared.lock() else {
                    return;
                };
                state.show_requested = true;
                state.waker.clone()
            };
            if let Some(waker) = waker {
                waker();
            }
        });
    }

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("SteamIdle")
            .with_inner_size([1060.0, 680.0])
            .with_min_inner_size([880.0, 560.0])
            // Tray mode parks the dashboard far off-screen instead of calling
            // ShowWindow(SW_HIDE): eframe repaints OS-hidden windows at 10 Hz,
            // which would keep an idle tray app awake. A visible but parked
            // window only paints when we ask it to.
            .with_position([-32000.0, -32000.0])
            .with_taskbar(false)
            .with_active(false)
            .with_icon(egui::IconData {
                rgba: icon::render(64),
                width: 64,
                height: 64,
            }),
        ..Default::default()
    };

    eframe::run_native(
        "SteamIdle",
        options,
        Box::new(move |cc| Ok(Box::new(app::App::new(cc, engine, show)))),
    )
    .map_err(|err| format!("graphics startup failed: {err}"))?;

    Ok(())
}
