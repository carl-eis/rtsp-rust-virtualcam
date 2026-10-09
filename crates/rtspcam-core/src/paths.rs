//! Standard per-user locations, from the `directories` crate.
//!
//! | | Windows | Linux | macOS |
//! |---|---|---|---|
//! | [`config_dir`] | `%APPDATA%\RtspCam` | `$XDG_CONFIG_HOME/RtspCam` (`~/.config/RtspCam`) | `~/Library/Application Support/RtspCam` |
//! | [`log_dir`] | `%LOCALAPPDATA%\RtspCam\logs` | `$XDG_DATA_HOME/RtspCam/logs` (`~/.local/share/RtspCam/logs`) | `~/Library/Application Support/RtspCam/logs` |

use std::path::PathBuf;

use crate::constants::{APP_DIR_NAME, CONFIG_FILE_NAME};
use crate::error::ConfigError;

fn base_dirs() -> Result<directories::BaseDirs, ConfigError> {
    directories::BaseDirs::new().ok_or(ConfigError::NoAppDataDir)
}

/// The folder that holds `config.json` (roaming on Windows).
pub fn config_dir() -> Result<PathBuf, ConfigError> {
    Ok(base_dirs()?.config_dir().join(APP_DIR_NAME))
}

/// `config.json` in [`config_dir`].
pub fn config_file() -> Result<PathBuf, ConfigError> {
    Ok(config_dir()?.join(CONFIG_FILE_NAME))
}

/// The app's rolling log files (local, not roaming, on Windows).
pub fn log_dir() -> Result<PathBuf, ConfigError> {
    Ok(base_dirs()?
        .data_local_dir()
        .join(APP_DIR_NAME)
        .join("logs"))
}
