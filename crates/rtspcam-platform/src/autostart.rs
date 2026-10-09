//! Starting the app when the user signs in.

use std::io;

/// The argument the autostart entry passes, so the app starts hidden in the tray (or
/// minimized, if "Minimize to tray" is off).
pub const MINIMIZED_ARG: &str = "--minimized";

/// Starts the app with [`MINIMIZED_ARG`] when the user signs in. Per user; no administrator
/// rights.
///
/// - **Windows**: the value `RtspCam` in `HKCU\Software\Microsoft\Windows\CurrentVersion\Run`.
/// - **Linux**: `rtspcam.desktop` in `$XDG_CONFIG_HOME/autostart` (XDG Autostart).
/// - **macOS**: a LaunchAgent plist in `~/Library/LaunchAgents` with `RunAtLoad`.
pub trait Autostart: Send + Sync {
    /// The setting's name in the UI, for example "Start with Windows".
    fn label(&self) -> &'static str;

    /// Whether the entry exists.
    fn is_enabled(&self) -> bool;

    /// Adds the entry (for the running executable) or removes it. Removing a missing entry
    /// is not an error.
    fn set(&self, enabled: bool) -> io::Result<()>;
}
