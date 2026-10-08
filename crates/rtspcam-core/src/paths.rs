//! Standard per-user locations.

use std::path::PathBuf;

use crate::constants::{APP_DIR_NAME, CONFIG_FILE_NAME};
use crate::error::ConfigError;

fn base_dirs() -> Result<directories::BaseDirs, ConfigError> {
    directories::BaseDirs::new().ok_or(ConfigError::NoAppDataDir)
}

/// `%APPDATA%\RtspCam` (roaming): holds `config.json`.
pub fn config_dir() -> Result<PathBuf, ConfigError> {
    Ok(base_dirs()?.config_dir().join(APP_DIR_NAME))
}

/// `%APPDATA%\RtspCam\config.json`.
pub fn config_file() -> Result<PathBuf, ConfigError> {
    Ok(config_dir()?.join(CONFIG_FILE_NAME))
}

/// `%LOCALAPPDATA%\RtspCam\logs` (local, not roaming): the app's rolling log files.
pub fn log_dir() -> Result<PathBuf, ConfigError> {
    Ok(base_dirs()?.data_local_dir().join(APP_DIR_NAME).join("logs"))
}
