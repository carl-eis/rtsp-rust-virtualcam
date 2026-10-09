use std::fmt;
use std::fs;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use super::Config;
use super::migrate::{CURRENT_VERSION, migrate};
use crate::error::ConfigError;

/// Puts a newly written file in place of an existing one, keeping the old one as a backup.
///
/// [`CopyThenRename`] works everywhere. `rtspcam-platform` has the Windows version
/// (`ReplaceFileW`, which keeps the file's ACL and attributes); pass it with
/// [`ConfigStore::with_replacer`].
pub trait FileReplace: Send + Sync + fmt::Debug {
    /// Moves `tmp` over the existing `target`, keeping the old `target` as `backup`.
    fn replace(&self, tmp: &Path, target: &Path, backup: &Path) -> io::Result<()>;
}

/// Copies `target` to `backup`, then renames `tmp` over `target`. The rename is atomic on one
/// file system, so `target` is never missing or half-written.
#[derive(Debug, Default, Clone, Copy)]
pub struct CopyThenRename;

impl FileReplace for CopyThenRename {
    fn replace(&self, tmp: &Path, target: &Path, backup: &Path) -> io::Result<()> {
        fs::copy(target, backup)?;
        fs::rename(tmp, target)
    }
}

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
    replacer: Arc<dyn FileReplace>,
}

impl ConfigStore {
    /// A store for the user's `config.json` (see [`crate::paths::config_file`]).
    pub fn open_default() -> Result<Self, ConfigError> {
        Ok(Self::new(crate::paths::config_file()?))
    }

    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            last_seen: Arc::default(),
            replacer: Arc::new(CopyThenRename),
        }
    }

    /// Uses `replacer` to put saved files in place (the default is [`CopyThenRename`]).
    #[must_use]
    pub fn with_replacer(mut self, replacer: Arc<dyn FileReplace>) -> Self {
        self.replacer = replacer;
        self
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
        if let Err(e) = self.replace(&tmp) {
            let _ = fs::remove_file(&tmp);
            return Err(ConfigError::io(&self.path, e));
        }
        tracing::debug!(path = %self.path.display(), "saved config");
        Ok(())
    }

    /// Moves `tmp` over `config.json`, keeping the old file as `config.json.bak`.
    fn replace(&self, tmp: &Path) -> io::Result<()> {
        if !self.path.exists() {
            return fs::rename(tmp, &self.path);
        }
        self.replacer.replace(tmp, &self.path, &self.backup_path())
    }

    fn remember(&self, bytes: &[u8]) {
        *self
            .last_seen
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(bytes.to_vec());
    }

    /// Records `bytes` as seen and returns `true` if they differ from the last content seen.
    #[cfg(feature = "watch")]
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

    /// Records each replacement, then does the default.
    #[derive(Debug, Default)]
    struct Recording(Mutex<Vec<PathBuf>>);

    impl FileReplace for Recording {
        fn replace(&self, tmp: &Path, target: &Path, backup: &Path) -> io::Result<()> {
            self.0.lock().unwrap().push(target.to_owned());
            CopyThenRename.replace(tmp, target, backup)
        }
    }

    #[test]
    fn an_existing_file_is_replaced_through_the_replacer() {
        let (_dir, store) = store();
        let recording = Arc::new(Recording::default());
        let store = store.with_replacer(recording.clone());
        store.save(&Config::default()).unwrap();
        assert!(
            recording.0.lock().unwrap().is_empty(),
            "nothing to replace yet"
        );
        store.save(&Config::default()).unwrap();
        assert_eq!(*recording.0.lock().unwrap(), [store.path().to_owned()]);
        assert!(store.backup_path().exists());
    }
}
