//! Platform abstraction. Only this folder knows about Win32, the registry or
//! `/proc`; the engine and UI use these functions. A future Linux build only
//! needs a complete `unix` implementation (the stubs here already cover the
//! core of it).

use std::path::PathBuf;

/// A running process, with its executable path when the OS lets us read it.
#[derive(Debug, Clone)]
pub struct ProcInfo {
    pub pid: u32,
    pub name: String,
    pub exe: Option<PathBuf>,
}

#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use windows::*;

#[cfg(not(windows))]
mod unix;
#[cfg(not(windows))]
pub use unix::*;
