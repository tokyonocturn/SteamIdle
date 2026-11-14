//! Win32 implementation of the platform layer.

use super::ProcInfo;
use std::ffi::OsString;
use std::os::windows::ffi::OsStringExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use windows_sys::core::BOOL;
use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_ALREADY_EXISTS, HANDLE, HWND, INVALID_HANDLE_VALUE, LPARAM, RECT,
    WAIT_OBJECT_0,
};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
};
use windows_sys::Win32::System::Threading::{
    CreateEventW, CreateMutexW, OpenEventW, OpenProcess, QueryFullProcessImageNameW, SetEvent,
    TerminateProcess, WaitForSingleObject, CREATE_NO_WINDOW, EVENT_MODIFY_STATE, INFINITE,
    PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_TERMINATE,
};
use windows_sys::Win32::UI::Shell::ShellExecuteW;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    EnumWindows, FindWindowW, GetWindowLongPtrW, GetWindowRect, GetWindowThreadProcessId,
    IsWindowVisible, MessageBoxW, PostMessageW, SetForegroundWindow, SetWindowLongPtrW, SetWindowPos,
    ShowWindow, SystemParametersInfoW, GWL_EXSTYLE, HWND_TOP, MB_ICONERROR, MB_OK, SPI_GETWORKAREA,
    SWP_FRAMECHANGED, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER, SW_RESTORE,
    SW_SHOWNORMAL, WM_CLOSE, WS_EX_APPWINDOW, WS_EX_TOOLWINDOW,
};

use winreg::enums::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE};
use winreg::RegKey;

/// Keeps the single-instance mutex alive for the lifetime of the process.
pub struct InstanceGuard {
    handle: HANDLE,
}

impl Drop for InstanceGuard {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.handle);
        }
    }
}

/// Returns `None` when another SteamIdle instance already holds the mutex.
pub fn single_instance() -> Option<InstanceGuard> {
    let name = wide("Local\\SteamIdle.SingleInstance");
    let handle = unsafe { CreateMutexW(std::ptr::null(), 0, name.as_ptr()) };
    if handle.is_null() {
        // If the mutex cannot be created we prefer running to failing silently.
        return Some(InstanceGuard {
            handle: std::ptr::null_mut(),
        });
    }
    if std::io::Error::last_os_error().raw_os_error() == Some(ERROR_ALREADY_EXISTS as i32) {
        unsafe {
            CloseHandle(handle);
        }
        return None;
    }
    Some(InstanceGuard { handle })
}

const SHOW_EVENT_NAME: &str = "Local\\SteamIdle.Show";

/// Called by a second launch: wakes the running instance so it can show its
/// dashboard on the UI thread. Falls back to a window lookup if the event was
/// never created.
pub fn request_show_existing() {
    let name = wide(SHOW_EVENT_NAME);
    unsafe {
        let event = OpenEventW(EVENT_MODIFY_STATE, 0, name.as_ptr());
        if event.is_null() {
            let title = wide("SteamIdle");
            let window = FindWindowW(std::ptr::null(), title.as_ptr());
            if !window.is_null() {
                ShowWindow(window, SW_RESTORE);
                SetForegroundWindow(window);
            }
            return;
        }
        SetEvent(event);
        CloseHandle(event);
    }
}

/// Keep the dashboard out of the taskbar and Alt+Tab. eframe's
/// `with_taskbar(false)` does not reach this window, and showing it can put
/// the app-window flag back, so this is checked again on every pass.
pub fn hide_from_taskbar(hwnd: isize) {
    if hwnd == 0 {
        return;
    }
    unsafe {
        let window = hwnd as HWND;
        let ex_style = GetWindowLongPtrW(window, GWL_EXSTYLE);
        let updated = (ex_style | WS_EX_TOOLWINDOW as isize) & !(WS_EX_APPWINDOW as isize);
        if updated == ex_style {
            return;
        }
        SetWindowLongPtrW(window, GWL_EXSTYLE, updated);
        SetWindowPos(
            window,
            std::ptr::null_mut(),
            0,
            0,
            0,
            0,
            SWP_FRAMECHANGED | SWP_NOACTIVATE | SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER,
        );
    }
}

/// Bring the dashboard on screen, centered in the primary work area. Using
/// Win32 pixels avoids DPI rounding differences between egui points and the
/// physical window position, which matters after the window was parked.
pub fn present_window(hwnd: isize) {
    if hwnd == 0 {
        return;
    }
    unsafe {
        let window = hwnd as HWND;
        ShowWindow(window, SW_RESTORE);
        let mut rect = RECT {
            left: 0,
            top: 0,
            right: 0,
            bottom: 0,
        };
        let mut work = RECT {
            left: 0,
            top: 0,
            right: 0,
            bottom: 0,
        };
        if GetWindowRect(window, &mut rect) != 0
            && SystemParametersInfoW(
                SPI_GETWORKAREA,
                0,
                &mut work as *mut RECT as *mut std::ffi::c_void,
                0,
            ) != 0
        {
            let width = rect.right - rect.left;
            let height = rect.bottom - rect.top;
            let x = work.left + ((work.right - work.left - width) / 2).max(0);
            let y = work.top + ((work.bottom - work.top - height) / 2).max(0);
            SetWindowPos(
                window,
                HWND_TOP,
                x,
                y,
                0,
                0,
                SWP_NOSIZE | SWP_NOACTIVATE,
            );
        }
        SetForegroundWindow(window);
        crate::logging::info("dashboard shown");
    }
}

/// Starts one idle thread that waits for show requests from later launches.
/// The thread blocks in the kernel, so it costs no CPU.
pub fn spawn_show_listener<F>(on_show: F)
where
    F: Fn() + Send + 'static,
{
    let name = wide(SHOW_EVENT_NAME);
    let event = unsafe { CreateEventW(std::ptr::null(), 0, 0, name.as_ptr()) };
    if event.is_null() {
        return;
    }
    // Raw handles are not `Send`; carry it as a usize into the thread.
    let raw = event as usize;
    let _ = std::thread::Builder::new()
        .name("steamidle-show".to_string())
        .spawn(move || loop {
            let event = raw as HANDLE;
            let result = unsafe { WaitForSingleObject(event, INFINITE) };
            if result == WAIT_OBJECT_0 {
                on_show();
            }
        });
}

/// Enumerate processes. Executable paths are loaded on demand by
/// [`process_image_path`] and cached by the caller, because querying every
/// path is the expensive part of a process scan.
pub fn list_processes() -> Vec<ProcInfo> {
    let mut processes = Vec::new();
    unsafe {
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snapshot == INVALID_HANDLE_VALUE || snapshot.is_null() {
            return processes;
        }

        let mut entry: PROCESSENTRY32W = std::mem::zeroed();
        entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;

        if Process32FirstW(snapshot, &mut entry) != 0 {
            loop {
                let end = entry
                    .szExeFile
                    .iter()
                    .position(|&unit| unit == 0)
                    .unwrap_or(entry.szExeFile.len());
                let name = String::from_utf16_lossy(&entry.szExeFile[..end]);
                processes.push(ProcInfo {
                    pid: entry.th32ProcessID,
                    name,
                    exe: None,
                });
                if Process32NextW(snapshot, &mut entry) == 0 {
                    break;
                }
            }
        }

        CloseHandle(snapshot);
    }
    processes
}

/// Full image path for one process, when access is allowed.
pub fn process_image_path(pid: u32) -> Option<PathBuf> {
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle.is_null() {
            return None;
        }
        let mut buffer = vec![0u16; 32_768];
        let mut length = buffer.len() as u32;
        let ok = QueryFullProcessImageNameW(handle, 0, buffer.as_mut_ptr(), &mut length);
        CloseHandle(handle);
        if ok == 0 || length == 0 {
            return None;
        }
        Some(PathBuf::from(OsString::from_wide(
            &buffer[..length as usize],
        )))
    }
}

/// Open a URL or folder with the user's default handler.
pub fn open_url(url: &str) -> Result<(), String> {
    let verb = wide("open");
    let target = wide(url);
    let result = unsafe {
        ShellExecuteW(
            std::ptr::null_mut(),
            verb.as_ptr(),
            target.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            SW_SHOWNORMAL,
        )
    };
    if (result as isize) <= 32 {
        Err(format!(
            "Windows could not open '{url}' (ShellExecute error {})",
            result as isize
        ))
    } else {
        Ok(())
    }
}

pub fn open_path(path: &Path) -> Result<(), String> {
    open_url(&path.to_string_lossy())
}

/// Start a GUI program without a console window and without waiting for it.
pub fn spawn_detached(exe: &Path, args: &[&str]) -> Result<(), String> {
    use std::os::windows::process::CommandExt;
    Command::new(exe)
        .args(args)
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .map(|_| ())
        .map_err(|err| format!("start {}: {err}", exe.display()))
}

/// Steam install paths from the registry plus the usual suspects.
pub fn steam_install_candidates() -> Vec<PathBuf> {
    let mut candidates = Vec::new();

    if let Some(path) = registry_path(
        RegKey::predef(HKEY_CURRENT_USER),
        r"Software\Valve\Steam",
        "SteamPath",
    ) {
        push_unique(&mut candidates, path);
    }
    if let Some(path) = registry_path(
        RegKey::predef(HKEY_LOCAL_MACHINE),
        r"SOFTWARE\WOW6432Node\Valve\Steam",
        "InstallPath",
    ) {
        push_unique(&mut candidates, path);
    }
    if let Some(path) = registry_path(
        RegKey::predef(HKEY_LOCAL_MACHINE),
        r"SOFTWARE\Valve\Steam",
        "InstallPath",
    ) {
        push_unique(&mut candidates, path);
    }

    for variable in ["ProgramFiles(x86)", "ProgramFiles"] {
        if let Some(base) = std::env::var_os(variable) {
            push_unique(&mut candidates, PathBuf::from(base).join("Steam"));
        }
    }
    for drive in ['C', 'D', 'E', 'F', 'G', 'H'] {
        for folder in ["Steam", "SteamLibrary", r"Games\Steam"] {
            push_unique(&mut candidates, PathBuf::from(format!("{drive}:\\{folder}")));
        }
    }
    candidates
}

fn registry_path(hive: RegKey, subkey: &str, value: &str) -> Option<PathBuf> {
    let raw = hive
        .open_subkey(subkey)
        .ok()?
        .get_value::<String, _>(value)
        .ok()?;
    let cleaned = raw.replace('/', "\\");
    let trimmed = cleaned.trim().trim_matches('"');
    (!trimmed.is_empty()).then(|| PathBuf::from(trimmed))
}

fn push_unique(list: &mut Vec<PathBuf>, path: PathBuf) {
    let normalized = path.to_string_lossy().to_lowercase().replace('/', "\\");
    if list
        .iter()
        .any(|existing| existing.to_string_lossy().to_lowercase().replace('/', "\\") == normalized)
    {
        return;
    }
    list.push(path);
}

struct CloseContext {
    pid: u32,
    windows: *mut Vec<HWND>,
}

unsafe extern "system" fn close_window_proc(window: HWND, lparam: LPARAM) -> BOOL {
    let context = &mut *(lparam as *mut CloseContext);
    let mut pid = 0u32;
    GetWindowThreadProcessId(window, &mut pid);
    if pid == context.pid && IsWindowVisible(window) != 0 {
        (*context.windows).push(window);
    }
    1
}

/// Politely ask the visible windows of these processes to close.
pub fn request_close(pids: &[u32]) {
    for pid in pids {
        let mut windows: Vec<HWND> = Vec::new();
        let mut context = CloseContext {
            pid: *pid,
            windows: &mut windows,
        };
        unsafe {
            EnumWindows(
                Some(close_window_proc),
                &mut context as *mut CloseContext as LPARAM,
            );
            for window in windows {
                PostMessageW(window, WM_CLOSE, 0, 0);
            }
        }
    }
}

/// Last resort used only by a queue slot that outlived its dwell time.
pub fn terminate(pid: u32) {
    unsafe {
        let handle = OpenProcess(PROCESS_TERMINATE, 0, pid);
        if !handle.is_null() {
            TerminateProcess(handle, 1);
            CloseHandle(handle);
        }
    }
}

/// Show a blocking error dialog. Used only when startup fails before the tray
/// exists, so a GUI-subsystem executable is not silent about it.
pub fn fatal_error(message: &str) {
    let text = wide(message);
    let title = wide("SteamIdle");
    unsafe {
        MessageBoxW(
            std::ptr::null_mut(),
            text.as_ptr(),
            title.as_ptr(),
            MB_OK | MB_ICONERROR,
        );
    }
}

fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}
