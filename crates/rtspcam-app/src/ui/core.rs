//! What the window code shares: the config, and the cameras it controls. Lives on the UI thread.

use std::cell::RefCell;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use rtspcam_core::config::{ConfigStore, ConfigWatcher};
use rtspcam_core::{Config, ConfigError};
use rtspcam_engine::{CameraManager, CameraStatus, Preview};
use tokio::runtime::Handle;
use uuid::Uuid;

/// Quitting must end the process within this long, whatever is stuck.
pub(crate) const SHUTDOWN_LIMIT: Duration = Duration::from_secs(3);

pub(crate) struct Core {
    store: ConfigStore,
    pub(crate) config: RefCell<Config>,
    manager: RefCell<Option<CameraManager>>,
    /// A config read from disk after someone edited the file by hand.
    external: Arc<Mutex<Option<Config>>>,
    _watcher: Option<ConfigWatcher>,
}

impl Core {
    pub(crate) fn new(store: ConfigStore, config: Config, manager: CameraManager) -> Self {
        manager.apply(&config);
        let external = Arc::new(Mutex::new(None));
        let watcher = {
            let external = external.clone();
            store
                .watch(move |result| match result {
                    Ok(config) => {
                        tracing::info!("config.json changed on disk");
                        *external.lock().unwrap_or_else(PoisonError::into_inner) = Some(config);
                    }
                    Err(e) => {
                        tracing::error!(error = %e, "config.json is invalid; keeping the old one");
                    }
                })
                .map_err(|e| tracing::warn!(error = %e, "not watching config.json"))
                .ok()
        };
        Self {
            store,
            config: RefCell::new(config),
            manager: RefCell::new(Some(manager)),
            external,
            _watcher: watcher,
        }
    }

    fn with_manager<T>(&self, f: impl FnOnce(&CameraManager) -> T) -> Option<T> {
        self.manager.borrow().as_ref().map(f)
    }

    pub(crate) fn statuses(&self) -> Vec<CameraStatus> {
        self.with_manager(CameraManager::statuses)
            .unwrap_or_default()
    }

    pub(crate) fn status(&self, id: Uuid) -> Option<CameraStatus> {
        self.with_manager(|m| m.status(id)).flatten()
    }

    pub(crate) fn open_preview(&self, id: Uuid) -> Option<Preview> {
        self.with_manager(|m| m.open_preview(id)).flatten()
    }

    pub(crate) fn is_paused(&self) -> bool {
        self.with_manager(CameraManager::is_paused).unwrap_or(false)
    }

    pub(crate) fn set_paused(&self, paused: bool) {
        self.with_manager(|m| m.set_paused(paused));
    }

    /// The manager's async runtime, for background work started by dialogs.
    pub(crate) fn runtime(&self) -> Option<Handle> {
        self.with_manager(|m| m.handle().clone())
    }

    /// Saves `config`, then makes the cameras match it.
    pub(crate) fn save(&self, config: Config) -> Result<(), ConfigError> {
        self.store.save(&config)?;
        self.with_manager(|m| m.apply(&config));
        *self.config.borrow_mut() = config;
        Ok(())
    }

    /// A config edited by hand since the last call, already applied to the cameras.
    pub(crate) fn take_external_change(&self) -> Option<Config> {
        let config = self
            .external
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()?;
        self.with_manager(|m| m.apply(&config));
        Some(config)
    }

    /// Stops everything (see [`CameraManager::shutdown`]).
    pub(crate) fn shutdown(&self) {
        if let Some(manager) = self.manager.borrow_mut().take() {
            manager.shutdown(SHUTDOWN_LIMIT);
        }
    }
}
