//! Optional "start with Windows" registration. Disabled by default; the user
//! has to turn it on in Settings. Only a per-user registry value is used, so no
//! administrator rights are involved.

#[cfg(windows)]
mod imp {
    use super::AUTOSTART_VALUE_NAME;
    use winreg::enums::{HKEY_CURRENT_USER, KEY_READ, KEY_WRITE};
    use winreg::RegKey;

    const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";

    pub fn is_enabled() -> bool {
        RegKey::predef(HKEY_CURRENT_USER)
            .open_subkey_with_flags(RUN_KEY, KEY_READ)
            .and_then(|key| key.get_value::<String, _>(AUTOSTART_VALUE_NAME))
            .is_ok()
    }

    pub fn set_enabled(enabled: bool) -> Result<(), String> {
        let key = RegKey::predef(HKEY_CURRENT_USER)
            .open_subkey_with_flags(RUN_KEY, KEY_READ | KEY_WRITE)
            .map_err(|err| format!("open the Windows startup key: {err}"))?;

        if enabled {
            let command = format!(
                "\"{}\" --tray",
                std::env::current_exe()
                    .map(|path| path.display().to_string())
                    .unwrap_or_else(|_| "steamidle.exe".to_string())
            );
            key.set_value(AUTOSTART_VALUE_NAME, &command)
                .map_err(|err| format!("write the Windows startup entry: {err}"))
        } else {
            match key.delete_value(AUTOSTART_VALUE_NAME) {
                Ok(()) => Ok(()),
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(err) => Err(format!("remove the Windows startup entry: {err}")),
            }
        }
    }
}

#[cfg(not(windows))]
mod imp {
    use std::fs;
    use std::path::PathBuf;

    fn autostart_file() -> Option<PathBuf> {
        let config = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))?;
        Some(config.join("autostart").join("steamidle.desktop"))
    }

    pub fn is_enabled() -> bool {
        autostart_file().map(|file| file.is_file()).unwrap_or(false)
    }

    pub fn set_enabled(enabled: bool) -> Result<(), String> {
        let Some(file) = autostart_file() else {
            return Err("cannot determine the desktop autostart folder".to_string());
        };
        if !enabled {
            return match fs::remove_file(&file) {
                Ok(()) => Ok(()),
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(err) => Err(format!("remove {}: {err}", file.display())),
            };
        }
        if let Some(parent) = file.parent() {
            fs::create_dir_all(parent).map_err(|err| format!("create {}: {err}", parent.display()))?;
        }
        let exe = std::env::current_exe()
            .map_err(|err| format!("locate the executable: {err}"))?
            .display()
            .to_string();
        let entry = format!(
            "[Desktop Entry]\nType=Application\nName=SteamIdle\nComment=Steam session companion\nExec=\"{exe}\" --tray\nTerminal=false\nX-GNOME-Autostart-enabled=true\n"
        );
        fs::write(&file, entry).map_err(|err| format!("write {}: {err}", file.display()))
    }
}

pub const AUTOSTART_VALUE_NAME: &str = "SteamIdle";

pub use imp::{is_enabled, set_enabled};

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use winreg::enums::{HKEY_CURRENT_USER, KEY_READ, KEY_WRITE};
    use winreg::RegKey;

    const TEST_VALUE: &str = "SteamIdleAutostartTest";

    #[test]
    fn startup_value_roundtrip() {
        let key = RegKey::predef(HKEY_CURRENT_USER)
            .open_subkey_with_flags(
                r"Software\Microsoft\Windows\CurrentVersion\Run",
                KEY_READ | KEY_WRITE,
            )
            .expect("startup key");

        key.set_value(TEST_VALUE, &"\"C:\\test\\steamidle.exe\" --tray")
            .expect("write test startup value");
        let read: String = key.get_value(TEST_VALUE).expect("read test value");
        assert!(read.contains("steamidle.exe"));
        key.delete_value(TEST_VALUE).expect("cleanup test value");

        // The real entry must not exist on a machine that never enabled it.
        let real = key.get_value::<String, _>(AUTOSTART_VALUE_NAME);
        if real.is_err() {
            assert!(!is_enabled());
        }
    }
}
