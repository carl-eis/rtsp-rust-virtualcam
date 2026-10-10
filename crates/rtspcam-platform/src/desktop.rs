//! Small desktop helpers that differ per OS.

use std::io;
use std::path::Path;

use crate::os;

/// Opens `folder` in the file manager (Explorer, Finder, or whatever `xdg-open` picks).
pub fn open_folder(folder: &Path) -> io::Result<()> {
    os::open_folder(folder)
}

/// Lets a GUI program print to the terminal it was started from. Windows only: a windowed
/// Windows program has no console of its own. Does nothing elsewhere.
pub fn attach_parent_console() {
    os::attach_parent_console();
}

/// Memory use of this process: (working set or resident size, private bytes), if this OS
/// reports it.
pub fn process_memory() -> Option<(usize, usize)> {
    os::process_memory()
}

/// Resolves when the system asks the process to end, other than Ctrl+C: SIGTERM (logout,
/// shutdown, `kill`) or SIGHUP on Linux and macOS. Never on Windows. Call it inside a tokio
/// runtime; the signals are caught from the first poll on, so start waiting early.
pub async fn termination_requested() -> io::Result<()> {
    os::termination_requested().await
}
