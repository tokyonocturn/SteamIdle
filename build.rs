//! Embeds the Windows icon resource so the portable .exe has a proper icon.
//!
//! Works with the MSVC toolchain (rc.exe from the Windows SDK) and the GNU
//! toolchain (windres from MinGW). If no resource compiler is available the
//! build still succeeds; the executable just keeps the default icon.

use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=assets/steamidle.rc");
    println!("cargo:rerun-if-changed=assets/steamidle.ico");
    println!("cargo:rerun-if-changed=build.rs");

    if std::env::var("CARGO_CFG_TARGET_OS").ok().as_deref() != Some("windows") {
        return;
    }

    let rc_file = Path::new("assets/steamidle.rc");
    if !rc_file.is_file() {
        println!("cargo:warning=assets/steamidle.rc missing; building without an exe icon");
        return;
    }

    let is_gnu = std::env::var("CARGO_CFG_TARGET_ENV")
        .map(|env| env == "gnu")
        .unwrap_or(false);

    let tool = if is_gnu {
        find_in_path("windres.exe").or_else(find_mingw_windres)
    } else {
        find_in_path("rc.exe").or_else(find_windows_sdk_rc)
    };

    let Some(tool) = tool else {
        println!(
            "cargo:warning=Windows resource compiler not found; building without an exe icon"
        );
        return;
    };

    let out_dir = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR"));
    let output = out_dir.join(if is_gnu {
        "steamidle_res.o"
    } else {
        "steamidle_res.res"
    });

    let mut cmd = Command::new(&tool);
    if is_gnu {
        cmd.current_dir("assets")
            .args(["-O", "coff", "-o"])
            .arg(&output)
            .arg("steamidle.rc");
    } else {
        cmd.arg("/nologo").arg("/fo").arg(&output).arg(rc_file);
    }

    match cmd.status() {
        Ok(status) if status.success() => {
            println!("cargo:rustc-link-arg={}", output.display());
        }
        Ok(status) => println!(
            "cargo:warning=resource compiler failed with {status}; building without an exe icon"
        ),
        Err(err) => println!(
            "cargo:warning=could not run {}: {err}; building without an exe icon",
            tool.display()
        ),
    }
}

fn find_in_path(file_name: &str) -> Option<PathBuf> {
    let paths = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&paths) {
        let candidate = dir.join(file_name);
        if candidate.is_file() {
            return Some(candidate);
        }
        if file_name.ends_with(".exe") {
            let bare = dir.join(file_name.trim_end_matches(".exe"));
            if bare.is_file() {
                return Some(bare);
            }
        }
    }
    None
}

fn find_mingw_windres() -> Option<PathBuf> {
    let gcc = find_in_path("gcc.exe")?;
    let candidate = gcc.with_file_name("windres.exe");
    candidate.is_file().then_some(candidate)
}

fn find_windows_sdk_rc() -> Option<PathBuf> {
    let kits = std::env::var_os("ProgramFiles(x86)").map(PathBuf::from)?;
    let bin = kits.join("Windows Kits").join("10").join("bin");
    let mut versions: Vec<PathBuf> = std::fs::read_dir(&bin)
        .ok()?
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|path| path.is_dir())
        .collect();
    versions.sort();
    versions.reverse();
    for version in versions {
        for arch in ["x64", "x86"] {
            let candidate = version.join(arch).join("rc.exe");
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}
