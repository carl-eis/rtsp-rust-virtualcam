use std::fs;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use super::Config;
use super::migrate::{CURRENT_VERSION, migrate};
use crate::error::ConfigError;

/// Reads and writes `config.json`.
///
/// Saves are atomic: the new content goes to `config.json.tmp` and then replaces
/// `config.json`, and the previous file is kept as `config.json.bak`.
///
/// Cloning is cheap; clones share the record of the last content read or written, which lets
/// the [watcher](ConfigStore::watch) ignore the app's own saves.
#[derive(Debug, Clone)]
pub struct ConfigStore {
    path: PathBuf,
    pub(super) last_seen: Arc<Mutex<Option<Vec<u8>>>>,
}

impl ConfigStore {
    /// A store for `%APPDATA%\RtspCam\config.json`.
    pub fn open_default() -> Result<Self, ConfigError> {
        Ok(Self::new(crate::paths::config_file()?))
    }

    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            last_seen: Arc::default(),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn backup_path(&self) -> PathBuf {
        with_suffix(&self.path, ".bak")
    }

    fn temp_path(&self) -> PathBuf {
        with_suffix(&self.path, ".tmp")
    }

    /// Loads the config. A missing file gives the default config.
    ///
    /// Files written by an older schema version are migrated and saved back in the new format
    /// (the original stays in `config.json.bak`).
    pub fn load(&self) -> Result<Config, ConfigError> {
        let bytes = match fs::read(&self.path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                tracing::info!(path = %self.path.display(), "no config file yet, using defaults");
                return Ok(Config::default());
            }
            Err(e) => return Err(ConfigError::io(&self.path, e)),
        };
        self.remember(&bytes);
        let (config, from_version) = parse(&self.path, &bytes)?;
        if from_version < CURRENT_VERSION {
            tracing::info!(
                from_version,
                to_version = CURRENT_VERSION,
                "migrated config"
            );
            self.save(&config)?;
        }
        Ok(config)
    }

    /// Writes the config atomically, keeping the previous file as `config.json.bak`.
    pub fn save(&self, config: &Config) -> Result<(), ConfigError> {
        let mut bytes = serde_json::to_vec_pretty(config).map_err(ConfigError::Serialize)?;
        bytes.push(b'\n');

        if let Some(dir) = self.path.parent() {
            fs::create_dir_all(dir).map_err(|e| ConfigError::io(dir, e))?;
        }
        let tmp = self.temp_path();
        write_synced(&tmp, &bytes).map_err(|e| ConfigError::io(&tmp, e))?;
        // Record before the rename so a watcher that fires right away sees our own write.
        self.remember(&bytes);
        if let Err(e) = replace(&tmp, &self.path, &self.backup_path()) {
            let _ = fs::remove_file(&tmp);
            return Err(ConfigError::io(&self.path, e));
        }
        tracing::debug!(path = %self.path.display(), "saved config");
        Ok(())
    }

    fn remember(&self, bytes: &[u8]) {
        *self
            .last_seen
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(bytes.to_vec());
    }

    /// Records `bytes` as seen and returns `true` if they differ from the last content seen.
    pub(super) fn remember_if_changed(&self, bytes: &[u8]) -> bool {
        let mut last = self
            .last_seen
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if last.as_deref() == Some(bytes) {
            return false;
        }
        *last = Some(bytes.to_vec());
        true
    }
}

/// Parses and migrates config bytes; returns the config and the version found in the file.
pub(super) fn parse(path: &Path, bytes: &[u8]) -> Result<(Config, u64), ConfigError> {
    let parse_err = |source| ConfigError::Parse {
        path: path.to_owned(),
        source,
    };
    let mut doc: serde_json::Value = serde_json::from_slice(bytes).map_err(parse_err)?;
    let from_version = migrate(&mut doc)?;
    let config = serde_json::from_value(doc).map_err(parse_err)?;
    Ok((config, from_version))
}

fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(suffix);
    PathBuf::from(name)
}

fn write_synced(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut file = fs::File::create(path)?;
    file.write_all(bytes)?;
    file.sync_all()
}

/// Moves `tmp` over `target`, keeping the old `target` as `backup`.
fn replace(tmp: &Path, target: &Path, backup: &Path) -> io::Result<()> {
    if !target.exists() {
        return fs::rename(tmp, target);
    }
    #[cfg(windows)]
    {
        match replace_file(tmp, target, backup) {
            Ok(()) => return Ok(()),
            Err(e) => {
                tracing::debug!(error = %e, "ReplaceFileW failed, falling back to copy + rename")
            }
        }
    }
    fs::copy(target, backup)?;
    fs::rename(tmp, target)
}

#[cfg(windows)]
fn replace_file(tmp: &Path, target: &Path, backup: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt as _;

    use windows::Win32::Storage::FileSystem::{REPLACEFILE_IGNORE_MERGE_ERRORS, ReplaceFileW};
    use windows::core::PCWSTR;

    fn wide(p: &Path) -> Vec<u16> {
        p.as_os_str().encode_wide().chain(Some(0)).collect()
    }
    let (target, tmp, backup) = (wide(target), wide(tmp), wide(backup));
    // SAFETY: all three are NUL-terminated UTF-16 strings that outlive the call.
    unsafe {
        ReplaceFileW(
            PCWSTR(target.as_ptr()),
            PCWSTR(tmp.as_ptr()),
            PCWSTR(backup.as_ptr()),
            REPLACEFILE_IGNORE_MERGE_ERRORS,
            None,
            None,
        )
    }
    .map_err(|e| io::Error::from_raw_os_error(e.code().0))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Config, StreamConfig};

    fn store() -> (tempfile::TempDir, ConfigStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = ConfigStore::new(dir.path().join("RtspCam").join("config.json"));
        (dir, store)
    }

    #[test]
    fn missing_file_loads_defaults() {
        let (_dir, store) = store();
        assert_eq!(store.load().unwrap(), Config::default());
        assert!(!store.path().exists());
    }

    #[test]
    fn save_then_load_round_trips() {
        let (_dir, store) = store();
        let mut config = Config::default();
        config.settings.minimize_to_tray = true;
        config
            .streams
            .push(StreamConfig::new("Front Door", "192.168.1.50"));
        store.save(&config).unwrap();
        assert_eq!(store.load().unwrap(), config);
        assert!(!store.temp_path().exists());
    }

    #[test]
    fn save_keeps_previous_file_as_backup() {
        let (_dir, store) = store();
        let mut config = Config::default();
        store.save(&config).unwrap();
        assert!(!store.backup_path().exists());

        config.settings.start_with_windows = true;
        store.save(&config).unwrap();
        let backup: Config =
            serde_json::from_slice(&fs::read(store.backup_path()).unwrap()).unwrap();
        assert!(!backup.settings.start_with_windows);

        config.settings.minimize_to_tray = true;
        store.save(&config).unwrap();
        let backup: Config =
            serde_json::from_slice(&fs::read(store.backup_path()).unwrap()).unwrap();
        assert!(backup.settings.start_with_windows && !backup.settings.minimize_to_tray);
        assert_eq!(store.load().unwrap(), config);
    }

    #[test]
    fn invalid_json_is_an_error() {
        let (_dir, store) = store();
        fs::create_dir_all(store.path().parent().unwrap()).unwrap();
        fs::write(store.path(), "{ not json").unwrap();
        assert!(matches!(store.load(), Err(ConfigError::Parse { .. })));
    }

    #[test]
    fn wrong_types_are_an_error() {
        let (_dir, store) = store();
        fs::create_dir_all(store.path().parent().unwrap()).unwrap();
        fs::write(store.path(), r#"{ "streams": [{ "port": "abc" }] }"#).unwrap();
        assert!(matches!(store.load(), Err(ConfigError::Parse { .. })));
    }
}
