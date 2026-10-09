//! The native Windows interface, built with `winsafe`.
//!
//! [`run`] owns the whole GUI session: it builds the window, starts the camera manager, and
//! returns after the window closed and the cameras were shut down.

mod main_window;
mod preview;
mod settings_dialog;
mod stream_dialog;
mod tray;

use std::cell::RefCell;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use rtspcam_core::config::{ConfigStore, ConfigWatcher};
use rtspcam_core::{Config, ConfigError};
use tokio::runtime::Handle;
use uuid::Uuid;

use crate::manager::{CameraManager, Preview};
use crate::single_instance::InstanceGuard;
use crate::status::CameraStatus;

pub(crate) use main_window::MainWindow;

/// Quitting must end the process within this long, whatever is stuck.
const SHUTDOWN_LIMIT: Duration = Duration::from_secs(3);

/// What the window code shares: the config, and the cameras it controls.
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

    pub(crate) fn runtime(&self) -> Handle {
        self.with_manager(|m| m.handle().clone())
            .expect("the manager runs until the window closes")
    }

    /// Saves `config`, then makes the cameras match it.
    pub(crate) fn save(&self, config: Config) -> Result<(), ConfigError> {
        self.store.save(&config)?;
        self.with_manager(|m| m.apply(&config));
        *self.config.borrow_mut() = config;
        Ok(())
    }

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

/// Runs the window until it is closed. Returns the process exit code.
pub fn run(
    store: ConfigStore,
    config: Config,
    manager: CameraManager,
    mut instance: InstanceGuard,
    start_minimized: bool,
) -> anyhow::Result<i32> {
    let core = Core::new(store, config, manager);
    // A second launch asks this copy to come forward. The window handle only exists once the
    // window is created, so the request goes through a late-bound slot.
    let slot: main_window::ShowSlot = Arc::default();
    {
        let slot = slot.clone();
        instance.on_show(move || {
            if let Some(show) = slot.lock().unwrap_or_else(PoisonError::into_inner).as_ref() {
                show();
            }
        });
    }
    let code = MainWindow::run(core, start_minimized, slot).map_err(|e| anyhow::anyhow!("{e}"))?;
    drop(instance);
    Ok(code)
}
