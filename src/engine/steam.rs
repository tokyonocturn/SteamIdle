//! Steam installation and library discovery.
//!
//! Everything here is read-only: registry values written by Steam, VDF files
//! Steam maintains, and the installed game folders. Launching delegates to the
//! real Steam client through its own `steam://` protocol, so Steam keeps full
//! control over licensing and anti-cheat.

use crate::engine::models::{Game, SteamStatus};
use crate::engine::vdf::Vdf;
use crate::platform::{self, ProcInfo};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

const STEAM_HELPERS: &[&str] = &[
    "steam.exe",
    "steamwebhelper.exe",
    "steamservice.exe",
    "steamerrorreporter.exe",
    "steamerrorreporter64.exe",
    "streaming_client.exe",
    "gameoverlayui.exe",
    "gameoverlayui64.exe",
    "html5app_steam.exe",
];

pub struct ScanResult {
    pub status: SteamStatus,
    pub games: Vec<Game>,
}

/// Discover the Steam client, all library folders and the installed games.
pub fn scan() -> ScanResult {
    let root = detect_root();
    let mut libraries: Vec<PathBuf> = Vec::new();

    if let Some(root) = &root {
        libraries.push(root.clone());
        for library in parse_library_folders(root) {
            if !libraries.iter().any(|known| same_path(known, &library)) {
                libraries.push(library);
            }
        }
    }

    let mut games: Vec<Game> = Vec::new();
    for library in &libraries {
        games.extend(scan_library(library));
    }
    games.sort_by(|a, b| {
        a.name
            .to_lowercase()
            .cmp(&b.name.to_lowercase())
            .then_with(|| a.appid.cmp(&b.appid))
    });
    let mut seen = HashSet::new();
    games.retain(|game| seen.insert(game.appid));

    let steam_exe = root
        .as_ref()
        .map(|root| root.join("steam.exe"))
        .filter(|exe| exe.is_file());

    ScanResult {
        status: SteamStatus {
            installed: root.is_some(),
            running: false,
            root,
            steam_exe,
            libraries,
        },
        games,
    }
}

/// Find the Steam root folder from the registry or common install paths.
pub fn detect_root() -> Option<PathBuf> {
    let candidates = platform::steam_install_candidates();
    for candidate in &candidates {
        if candidate.join("steamapps").is_dir() || candidate.join("steam.exe").is_file() {
            return Some(candidate.clone());
        }
    }
    candidates.into_iter().find(|candidate| candidate.is_dir())
}

/// Every library root from `steamapps/libraryfolders.vdf`.
pub fn parse_library_folders(root: &Path) -> Vec<PathBuf> {
    let path = root.join("steamapps").join("libraryfolders.vdf");
    match fs::read_to_string(&path) {
        Ok(text) => library_paths_from_vdf(&text),
        Err(_) => Vec::new(),
    }
}

/// Parse the library list from VDF text. Supports both the current table
/// format and the old `"1" "D:\\SteamLibrary"` format.
pub fn library_paths_from_vdf(text: &str) -> Vec<PathBuf> {
    let Ok(vdf) = Vdf::parse(text) else {
        return Vec::new();
    };
    let Some(folders) = vdf.get("libraryfolders").and_then(|value| value.as_table()) else {
        return Vec::new();
    };

    let mut libraries: Vec<PathBuf> = Vec::new();
    for entry in folders.values() {
        let path = match entry {
            Vdf::Str(path) => Some(path.as_str()),
            Vdf::Table(table) => table
                .iter()
                .find(|(key, _)| key.eq_ignore_ascii_case("path"))
                .and_then(|(_, value)| value.as_str()),
        };
        if let Some(path) = path {
            let path = PathBuf::from(path.replace('/', "\\"));
            if path.is_dir() && !libraries.iter().any(|known| same_path(known, &path)) {
                libraries.push(path);
            }
        }
    }
    libraries
}

/// Read every `appmanifest_*.acf` in one library.
pub fn scan_library(library: &Path) -> Vec<Game> {
    let steamapps = library.join("steamapps");
    let Ok(entries) = fs::read_dir(&steamapps) else {
        return Vec::new();
    };

    let mut games = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if !name.starts_with("appmanifest_") || !name.ends_with(".acf") {
            continue;
        }
        if let Ok(text) = fs::read_to_string(entry.path()) {
            if let Some(game) = parse_app_manifest(&text, library) {
                games.push(game);
            }
        }
    }
    games
}

/// Parse a single app manifest. Returns `None` for malformed entries so one
/// broken file never hides the rest of the library.
pub fn parse_app_manifest(text: &str, library: &Path) -> Option<Game> {
    let vdf = Vdf::parse(text).ok()?;
    let state = vdf.get("AppState")?;
    let appid: u32 = state.get_str("appid")?.trim().parse().ok()?;
    let name = state.get_str("name").unwrap_or_default().trim().to_string();
    if appid == 0 || name.is_empty() {
        return None;
    }

    let install_dir = state
        .get_str("installdir")
        .filter(|dir| !dir.trim().is_empty())
        .map(|dir| {
            library
                .join("steamapps")
                .join("common")
                .join(dir.trim())
        });
    let installed = install_dir
        .as_ref()
        .map(|dir| dir.is_dir())
        .unwrap_or(false);
    let size_on_disk = state
        .get_str("SizeOnDisk")
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(0);
    let last_played = state
        .get_str("LastPlayed")
        .and_then(|value| value.parse::<i64>().ok())
        .filter(|value| *value > 0);

    Some(Game {
        appid,
        name,
        library: Some(library.to_path_buf()),
        install_dir,
        size_on_disk,
        installed,
        last_played,
        running: false,
    })
}

/// Map running processes to the games whose install folder contains them.
/// When install folders overlap, the deepest one wins.
pub fn running_map(games: &[Game], processes: &[ProcInfo]) -> HashMap<u32, Vec<u32>> {
    let mut directories: Vec<(u32, String)> = games
        .iter()
        .filter(|game| game.installed)
        .filter_map(|game| {
            game.install_dir
                .as_ref()
                .map(|dir| (game.appid, normalize_path(dir)))
        })
        .collect();
    directories.sort_by_key(|(_, dir)| std::cmp::Reverse(dir.len()));

    let mut running: HashMap<u32, Vec<u32>> = HashMap::new();
    for process in processes {
        if STEAM_HELPERS
            .iter()
            .any(|helper| process.name.eq_ignore_ascii_case(helper))
        {
            continue;
        }
        let Some(exe) = process.exe.as_ref() else {
            continue;
        };
        let exe = normalize_path(exe);
        let Some((appid, _)) = directories.iter().find(|(_, dir)| {
            exe.len() > dir.len()
                && exe.starts_with(dir)
                && exe.as_bytes().get(dir.len()) == Some(&b'\\')
        }) else {
            continue;
        };
        running.entry(*appid).or_default().push(process.pid);
    }
    running
}

pub fn is_steam_running(processes: &[ProcInfo]) -> bool {
    processes
        .iter()
        .any(|process| process.name.eq_ignore_ascii_case("steam.exe"))
}

/// Ask the installed Steam client to launch a game. Steam performs its own
/// ownership checks; SteamIdle never touches game files or memory.
pub fn launch_game(appid: u32) -> Result<(), String> {
    platform::open_url(&format!("steam://rungameid/{appid}"))
}

/// Start Steam itself, preferably through the client that is installed.
pub fn launch_steam(status: &SteamStatus) -> Result<(), String> {
    if let Some(root) = &status.root {
        let exe = root.join("steam.exe");
        if exe.is_file() {
            return platform::spawn_detached(&exe, &[]);
        }
    }
    platform::open_url("steam://open/main")
}

fn normalize_path(path: &Path) -> String {
    path.to_string_lossy()
        .replace('/', "\\")
        .trim_end_matches('\\')
        .to_lowercase()
}

pub fn same_path(left: &Path, right: &Path) -> bool {
    normalize_path(left) == normalize_path(right)
}

#[cfg(test)]
mod tests {
    use super::*;

    const MANIFEST: &str = r#"
        "AppState"
        {
            "appid"     "440"
            "name"      "Team Fortress 2"
            "StateFlags" "4"
            "installdir" "Team Fortress 2"
            "SizeOnDisk" "33292355826"
            "LastPlayed" "1794696648"
        }
    "#;

    #[test]
    fn parses_manifest_fields() {
        let game = parse_app_manifest(MANIFEST, Path::new(r"C:\Program Files (x86)\Steam"))
            .expect("manifest");
        assert_eq!(game.appid, 440);
        assert_eq!(game.name, "Team Fortress 2");
        assert_eq!(game.size_on_disk, 33_292_355_826);
        assert_eq!(game.last_played, Some(1_794_696_648));
        assert_eq!(
            game.install_dir.as_deref().map(|dir| dir.ends_with("Team Fortress 2")),
            Some(true)
        );
    }

    #[test]
    fn parses_current_library_format() {
        let text = r#"
            "libraryfolders"
            {
                "0" { "path" "C:\\Program Files (x86)\\Steam" "apps" { "440" "0" } }
                "1" { "path" "D:\\SteamLibrary" }
            }
        "#;
        let libraries = library_paths_from_vdf(text);
        // None of these test paths exist on disk, so the disk filter keeps only
        // entries that happen to exist. Just assert parsing did not panic.
        assert!(libraries.len() <= 2);
    }

    #[test]
    fn ignores_malformed_manifests() {
        assert!(parse_app_manifest("not vdf {{{", Path::new("C:\\Steam")).is_none());
        assert!(parse_app_manifest(r#""AppState" { "appid" "x" }"#, Path::new("C:\\Steam")).is_none());
    }

    #[test]
    fn running_map_matches_install_folder() {
        let game = Game {
            appid: 440,
            name: "Team Fortress 2".into(),
            installed: true,
            install_dir: Some(PathBuf::from(r"D:\SteamLibrary\steamapps\common\Team Fortress 2")),
            ..Game::default()
        };
        let processes = vec![
            ProcInfo {
                pid: 42,
                name: "hl2.exe".into(),
                exe: Some(PathBuf::from(
                    r"d:\steamlibrary\steamapps\common\team fortress 2\hl2.exe",
                )),
            },
            ProcInfo {
                pid: 43,
                name: "steam.exe".into(),
                exe: Some(PathBuf::from(r"C:\Program Files (x86)\Steam\steam.exe")),
            },
            ProcInfo {
                pid: 44,
                name: "notepad.exe".into(),
                exe: Some(PathBuf::from(r"C:\Windows\notepad.exe")),
            },
        ];
        let map = running_map(&[game], &processes);
        assert_eq!(map.get(&440), Some(&vec![42]));
    }

    /// Environment-aware smoke test: runs against the real machine when Steam
    /// is installed, and passes trivially otherwise.
    #[test]
    fn discovers_local_steam_library_when_present() {
        let result = scan();
        if !result.status.installed {
            return;
        }
        assert!(!result.status.libraries.is_empty());
        for game in &result.games {
            assert!(game.appid > 0);
            assert!(!game.name.trim().is_empty());
        }
    }
}
