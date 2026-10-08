use std::ffi::OsString;
use std::io;
use std::path::PathBuf;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread::{self, JoinHandle};
use std::time::Duration;
use std::{fmt, fs};

use notify::{RecommendedWatcher, RecursiveMode, Watcher as _};

use super::Config;
use super::store::{ConfigStore, parse};
use crate::error::ConfigError;

/// Editors and our own atomic save produce bursts of events; wait this long for quiet.
const DEBOUNCE: Duration = Duration::from_millis(300);

enum Msg {
    Fs(notify::Result<notify::Event>),
    Stop,
}

/// Watches `config.json` and calls back with the reloaded config when it changes on disk.
///
/// Saves made through the same [`ConfigStore`] (or a clone of it) are not reported. Invalid
/// JSON is reported as an `Err`, so the app can show it and keep using the last good config.
/// Dropping the watcher stops it.
pub struct ConfigWatcher {
    watcher: Option<RecommendedWatcher>,
    tx: mpsc::Sender<Msg>,
    thread: Option<JoinHandle<()>>,
}

impl fmt::Debug for ConfigWatcher {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ConfigWatcher").finish_non_exhaustive()
    }
}

impl ConfigStore {
    /// Starts watching the config file. `on_change` runs on a background thread.
    pub fn watch<F>(&self, on_change: F) -> Result<ConfigWatcher, ConfigError>
    where
        F: FnMut(Result<Config, ConfigError>) + Send + 'static,
    {
        // Watch the folder, not the file: an atomic replace swaps the file out from under a
        // file watch.
        let dir = self
            .path()
            .parent()
            .map(PathBuf::from)
            .unwrap_or_else(|| ".".into());
        fs::create_dir_all(&dir).map_err(|e| ConfigError::io(&dir, e))?;

        let (tx, rx) = mpsc::channel();
        let fs_tx = tx.clone();
        let mut watcher = notify::recommended_watcher(move |ev| {
            let _ = fs_tx.send(Msg::Fs(ev));
        })?;
        watcher.watch(&dir, RecursiveMode::NonRecursive)?;

        let store = self.clone();
        let thread = thread::Builder::new()
            .name("config-watch".into())
            .spawn(move || run(store, rx, on_change))
            .map_err(|e| ConfigError::io(&dir, e))?;

        Ok(ConfigWatcher {
            watcher: Some(watcher),
            tx,
            thread: Some(thread),
        })
    }
}

fn run<F>(store: ConfigStore, rx: mpsc::Receiver<Msg>, mut on_change: F)
where
    F: FnMut(Result<Config, ConfigError>),
{
    let file_name: Option<OsString> = store.path().file_name().map(Into::into);
    let concerns_config = |event: &notify::Event| {
        event
            .paths
            .iter()
            .any(|p| p.file_name().map(Into::into) == file_name)
    };

    loop {
        // Wait for a relevant event.
        match rx.recv() {
            Ok(Msg::Fs(Ok(event))) if concerns_config(&event) => {}
            Ok(Msg::Fs(Ok(_))) => continue,
            Ok(Msg::Fs(Err(e))) => {
                tracing::warn!(error = %e, "config watcher error");
                continue;
            }
            Ok(Msg::Stop) | Err(_) => return,
        }
        // Debounce: wait until events stop arriving.
        loop {
            match rx.recv_timeout(DEBOUNCE) {
                Ok(Msg::Fs(_)) => continue,
                Err(RecvTimeoutError::Timeout) => break,
                Ok(Msg::Stop) | Err(RecvTimeoutError::Disconnected) => return,
            }
        }

        let bytes = match fs::read(store.path()) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                tracing::debug!("config file removed; keeping current config");
                continue;
            }
            Err(e) => {
                on_change(Err(ConfigError::io(store.path(), e)));
                continue;
            }
        };
        if !store.remember_if_changed(&bytes) {
            continue;
        }
        tracing::info!(path = %store.path().display(), "config changed on disk, reloading");
        on_change(parse(store.path(), &bytes).map(|(config, _)| config));
    }
}

impl Drop for ConfigWatcher {
    fn drop(&mut self) {
        // Stop OS notifications first, then the worker thread.
        drop(self.watcher.take());
        let _ = self.tx.send(Msg::Stop);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc::Receiver;

    use super::*;

    const WAIT: Duration = Duration::from_secs(5);

    fn watched() -> (
        tempfile::TempDir,
        ConfigStore,
        ConfigWatcher,
        Receiver<Result<Config, ConfigError>>,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let store = ConfigStore::new(dir.path().join("config.json"));
        store.save(&Config::default()).unwrap();
        let (tx, rx) = mpsc::channel();
        let watcher = store
            .watch(move |res| {
                let _ = tx.send(res);
            })
            .unwrap();
        (dir, store, watcher, rx)
    }

    #[test]
    fn reports_external_edits() {
        let (_dir, store, _watcher, rx) = watched();
        fs::write(
            store.path(),
            r#"{ "settings": { "minimize_to_tray": true } }"#,
        )
        .unwrap();
        let config = rx.recv_timeout(WAIT).expect("no reload").unwrap();
        assert!(config.settings.minimize_to_tray);
    }

    #[test]
    fn reports_invalid_json_as_error() {
        let (_dir, store, _watcher, rx) = watched();
        fs::write(store.path(), "{ broken").unwrap();
        assert!(matches!(
            rx.recv_timeout(WAIT).expect("no reload"),
            Err(ConfigError::Parse { .. })
        ));
    }

    #[test]
    fn ignores_own_saves() {
        let (_dir, store, _watcher, rx) = watched();
        let mut config = Config::default();
        config.settings.start_with_windows = true;
        store.save(&config).unwrap();
        assert!(rx.recv_timeout(DEBOUNCE * 4).is_err());
    }
}
