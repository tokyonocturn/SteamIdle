# SteamIdle

A lightweight, portable Windows tray companion for your Steam library.

SteamIdle sits in the system tray, detects the games Steam has installed, launches them through the real Steam client, and records how long they actually run. Everything stays on your machine: one portable `.exe`, one local SQLite database, no installer, no telemetry.

> SteamIdle is an independent open-source tool. It never asks for Steam credentials, never impersonates the Steam client, never spoofs playtime, and never bypasses ownership checks or anti-cheat. It only observes processes Windows already exposes and asks Steam to launch games the normal way.

## Features

- **Tray first.** Starts minimized to the system tray with no console window. Left-click the icon to open the dashboard.
- **Tray menu:** Open, Settings, View Sessions, Start Steam, Exit.
- **Library discovery.** Reads the installed Steam client from the registry and `libraryfolders.vdf`, then every library's `appmanifest_*.acf`, so games from all drives appear.
- **Game inventory.** Names, AppIDs, install status, install size, tracked playtime, and whether the game is running right now.
- **Legitimate launching.** Games and Steam itself are started through the installed client (`steam://rungameid/<appid>`), so Steam keeps full control of licensing and protection.
- **Real session tracking.** A background worker polls the Windows process list, matches game executables inside their own install folders, and records each session with heartbeats.
- **Local history.** Recent sessions, totals per game, session count, and the last 24 hours, all stored in SQLite.
- **Launch queues.** Build an ordered list of installed games with a dwell time each, then run it. Useful for legitimately testing a setup; Steam performs every launch.
- **Crash-safe storage.** WAL-mode SQLite plus heartbeat timestamps. On the next start, sessions left open by a crash or power loss are closed at their last observed heartbeat instead of losing data or inventing time.
- **Portable settings.** Start with Windows (off by default, per-user registry entry, no admin rights), close-to-tray behaviour, and process poll interval.
- **Dark minimal dashboard.** Library, Sessions, Queues, and Settings in one compact window.
- **Lightweight.** No Electron, no WebView, no updater, no background services. Roughly 6.5 MB on disk and around 75–80 MB working set with the dashboard parked in the tray.

## Requirements

- Windows 10 or Windows 11, x64.
- The real Steam client installed and signed in at least once.
- Nothing else. The executable links the C runtime statically and needs no installer.

## Download and run

1. Take `steamidle.exe` from the `dist` folder (or build it yourself, see below).
2. Put it anywhere you like — a normal folder or a USB stick.
3. Double-click it. It starts in the system tray, hidden.
4. Left-click the tray icon, or use **Open SteamIdle**, to show the dashboard.

A second launch does not start a second tracker; it just brings the running dashboard forward.

### Tray menu

| Item | Action |
| --- | --- |
| Open SteamIdle | Show the dashboard (Library tab) |
| Settings | Show the dashboard on the Settings tab |
| View Sessions | Show the dashboard on the Sessions tab |
| Start Steam | Start the installed Steam client |
| Exit | Close open sessions, checkpoint the database, and quit |

### Data files

Everything lives next to the executable:

| File | Purpose |
| --- | --- |
| `steamidle.db` | SQLite database: games, sessions, queues, settings |
| `steamidle.db-wal`, `steamidle.db-shm` | SQLite WAL files while running |
| `steamidle.log` | Small local log, rotated at 2 MB |

If the executable folder is read-only (for example after dropping the exe into `Program Files`), SteamIdle falls back to `%LOCALAPPDATA%\SteamIdle` so it still works.

## How session tracking works

While SteamIdle runs, the worker samples the process list every few seconds (configurable, default 5 s):

1. It takes the process name and executable path of running processes.
2. A game counts as running when its executable lives inside that game's own `steamapps\common\<installdir>` folder. Steam helper processes are ignored.
3. A session row is inserted when the first process of a game appears, heartbeats are written while it runs, and the row is closed when the last matching process exits.
4. The database is in WAL mode, so an interrupted write never corrupts the history. Open sessions are closed at their last heartbeat on the next start.

SteamIdle never reads game memory, never modifies game files, never writes to Steam's own files, and never talks to Steam's network services.

## Launch queues

A queue is a name plus an ordered list of installed games and a dwell time per game. When you press **Run queue**, SteamIdle:

1. Asks the real Steam client to launch the next game.
2. Waits for the game process to appear (up to two minutes).
3. Lets it run for the configured number of minutes while the normal tracker records the session.
4. Sends a normal close request to the game's windows; if it is still running after a grace period, it ends the process it started.
5. Waits a few seconds, then moves to the next item.

Queues are intended for legitimate testing of your own library. They cannot add playtime to a game you do not own, and they do not interact with Steam's servers.

## Settings

- **Start SteamIdle with Windows** — off by default. Uses `HKCU\Software\Microsoft\Windows\CurrentVersion\Run`, so no administrator rights are required.
- **Closing the window hides it to the tray** — on by default. Turn it off if you prefer closing to exit.
- **Check running games every N seconds** — 2 to 30 seconds, default 5.
- **Data folder** — shows the exact database location and opens it in Explorer.

Startup registration can also be verified or removed from Task Manager → Startup apps.

## Build from source

### Prerequisites

- Rust stable 1.90 or newer (`rustup` recommended).
- On Windows with the MSVC toolchain: Visual Studio Build Tools with the "Desktop development with C++" workload (provides `link.exe` and the Windows SDK `rc.exe` used for the icon resource). The GNU toolchain with MinGW also works; the build script finds `windres`.
- No other system libraries: SQLite is compiled in (`rusqlite` bundled feature).

### Build

```powershell
cargo build --release
```

The portable executable is written to `target\release\steamidle.exe`.

If your default toolchain is GNU and you also have MSVC installed:

```powershell
cargo +stable-x86_64-pc-windows-msvc build --release
```

MSVC with `+crt-static` (already configured in `.cargo/config.toml`) produces an executable that needs only Windows system DLLs, which is why the shipped `steamidle.exe` is a single file.

### Test

```powershell
cargo test
```

The suite covers the VDF parser, app-manifest parsing, process-to-game matching, session storage and crash recovery, queue state transitions, settings persistence, and the icon renderer. One test also scans the local Steam installation when present and skips gracefully when Steam is not installed.

### Run during development

```powershell
cargo run -- --show
```

`--show` opens the dashboard immediately instead of starting in the tray. Without arguments the app behaves exactly like the release build: hidden tray mode.

### Icon resource

`assets/steamidle.ico` is embedded by `build.rs` through `rc.exe` (MSVC) or `windres` (GNU). If neither compiler is available, the build still succeeds and the executable simply uses the default icon; the tray icon is always drawn in code.

## Architecture

The code is split so the platform-independent logic can be reused for a future Linux build:

```
src/
├── main.rs              GUI-subsystem entry point, single instance, startup
├── app.rs               egui dashboard, tray-mode window handling
├── icon.rs              programmatic tray/window icon
├── logging.rs           small rotating local log
├── paths.rs             portable data directory with read-only fallback
├── engine/
│   ├── models.rs        games, sessions, queues, settings, formatting
│   ├── vdf.rs           dependency-free Valve Data Format parser
│   ├── db.rs            SQLite schema, session recovery, queues, settings
│   ├── steam.rs         Steam discovery, library/manifest parsing, launching
│   ├── queue.rs         deterministic launch-queue state machine
│   ├── autostart.rs     per-user startup entry (registry on Windows)
│   └── worker.rs        background thread: tracking, queues, shared state
├── platform/
│   ├── windows.rs       Win32 processes, ShellExecute, registry, single instance
│   └── unix.rs          /proc and xdg-open implementation for Linux
└── ui/
    ├── theme.rs         dark theme tokens and egui style
    └── tray.rs          native tray icon and menu
```

- The worker owns the only database connection, so the UI never blocks on SQLite.
- The UI reads an immutable snapshot and sends commands over a channel.
- All Win32 calls are confined to `platform/windows.rs` and the Windows-only parts of `engine/autostart.rs`; `platform/unix.rs` already implements the same interface for a future Linux target.

## Design notes and limitations

- **Process detection relies on executable paths.** Some heavily protected games do not let another process read their image path; those games will not be tracked automatically. Games launched by third-party launchers outside a Steam library folder are also not tracked.
- **Only one instance runs** so sessions are never double-counted.
- **Tray mode parks the dashboard off-screen.** eframe repaints OS-hidden windows at a fixed 10 Hz, which would keep a background utility awake; an off-screen but visible window only paints when the app requests it. The window is removed from the taskbar and is not part of Alt+Tab.
- **No playtime injection.** Steam's own recorded playtime is untouched. SteamIdle only records the runtime it observes locally.
- The dashboard is optional; tracking works with the window closed in the tray.

## License

MIT. See [LICENSE](LICENSE).
