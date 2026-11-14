//! System tray integration.
//!
//! The tray is created on the UI thread because Win32 delivers its messages
//! through the same event loop that eframe/winit runs. It lives as long as the
//! app; dropping it removes the icon.

/// Actions requested from the tray menu or a left click on the icon.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayAction {
    Open,
    Settings,
    Sessions,
    Exit,
}

#[cfg(windows)]
mod imp {
    use super::TrayAction;
    use crate::engine::worker::{Command, Shared};
    use crate::icon;
    use std::sync::mpsc::{channel, Receiver, Sender};
    use std::sync::{Arc, Mutex};
    use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
    use tray_icon::{Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};

    enum TrayEvent {
        Menu(String),
        LeftClick,
    }

    pub struct TrayHandle {
        tray: TrayIcon,
        events: Receiver<TrayEvent>,
        open: String,
        settings: String,
        sessions: String,
        exit: String,
    }

    impl TrayHandle {
        /// `ctx` wakes the (possibly hidden) UI for menu entries that need the
        /// dashboard. `Start Steam` is queued for the worker instead, so it
        /// never triggers a repaint of the hidden window.
        pub fn new(
            ctx: &egui::Context,
            shared: Arc<Mutex<Shared>>,
        ) -> Result<Self, String> {
            let open = MenuItem::with_id("open", "Open SteamIdle", true, None);
            let settings = MenuItem::with_id("settings", "Settings", true, None);
            let sessions = MenuItem::with_id("sessions", "View Sessions", true, None);
            let start_steam = MenuItem::with_id("start_steam", "Start Steam", true, None);
            let separator = PredefinedMenuItem::separator();
            let exit = MenuItem::with_id("exit", "Exit", true, None);

            let menu = Menu::new();
            menu.append_items(&[&open, &settings, &sessions, &start_steam, &separator, &exit])
                .map_err(|err| format!("build the tray menu: {err}"))?;

            let icon = Icon::from_rgba(icon::render(32), 32, 32)
                .map_err(|err| format!("build the tray icon: {err:?}"))?;

            let tray = TrayIconBuilder::new()
                .with_menu(Box::new(menu))
                .with_tooltip("SteamIdle — no game tracked")
                .with_icon(icon)
                .build()
                .map_err(|err| format!("create the tray icon: {err}"))?;

            let (sender, events) = channel::<TrayEvent>();
            install_handlers(
                sender,
                ctx.clone(),
                shared,
                start_steam.id().0.clone(),
            );

            Ok(Self {
                tray,
                events,
                open: open.id().0.clone(),
                settings: settings.id().0.clone(),
                sessions: sessions.id().0.clone(),
                exit: exit.id().0.clone(),
            })
        }

        /// Drain queued tray events without blocking the UI thread.
        pub fn poll(&self) -> Vec<TrayAction> {
            let mut actions = Vec::new();
            while let Ok(event) = self.events.try_recv() {
                match event {
                    TrayEvent::LeftClick => actions.push(TrayAction::Open),
                    TrayEvent::Menu(id) => {
                        let action = if id == self.open {
                            Some(TrayAction::Open)
                        } else if id == self.settings {
                            Some(TrayAction::Settings)
                        } else if id == self.sessions {
                            Some(TrayAction::Sessions)
                        } else if id == self.exit {
                            Some(TrayAction::Exit)
                        } else {
                            None
                        };
                        if let Some(action) = action {
                            actions.push(action);
                        }
                    }
                }
            }
            actions
        }

        pub fn set_tooltip(&self, tooltip: &str) {
            let _ = self.tray.set_tooltip(Some(tooltip.to_string()));
        }
    }

    /// Forward native tray events to our own channel and wake egui. The muda
    /// receivers do not deliver anything once a handler is installed, so the
    /// handler is the single source of truth from here on.
    fn install_handlers(
        sender: Sender<TrayEvent>,
        ctx: egui::Context,
        shared: Arc<Mutex<Shared>>,
        start_steam_id: String,
    ) {
        let menu_sender = sender.clone();
        let menu_ctx = ctx.clone();
        MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
            let id = event.id.0.clone();
            if id == start_steam_id {
                if let Ok(mut state) = shared.lock() {
                    state.pending.push(Command::StartSteam);
                }
                return;
            }
            let _ = menu_sender.send(TrayEvent::Menu(id));
            menu_ctx.request_repaint();
        }));

        TrayIconEvent::set_event_handler(Some(move |event: TrayIconEvent| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                let _ = sender.send(TrayEvent::LeftClick);
                ctx.request_repaint();
            }
        }));
    }
}

#[cfg(not(windows))]
mod imp {
    use super::TrayAction;

    pub struct TrayHandle;

    impl TrayHandle {
        pub fn new(
            _ctx: &egui::Context,
            _shared: std::sync::Arc<std::sync::Mutex<crate::engine::worker::Shared>>,
        ) -> Result<Self, String> {
            Err("the system tray is only implemented on Windows".to_string())
        }

        pub fn poll(&self) -> Vec<TrayAction> {
            Vec::new()
        }

        pub fn set_tooltip(&self, _tooltip: &str) {}
    }
}

pub use imp::TrayHandle;
