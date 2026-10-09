//! `rtspcam.exe`.
//!
//! `--headless` runs the camera manager without a window: the configured cameras exist until
//! Ctrl+C (or the console closing), and edits to `config.json` are applied live.

use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context as _;
use rtspcam_app::backend::VcamBackend;
use rtspcam_app::single_instance::{self, Instance};
use rtspcam_app::{CameraManager, ManagerOptions};
use rtspcam_core::config::ConfigStore;
use rtspcam_core::logging::{self, LogOptions};
use rtspcam_core::paths;

/// Quitting must end the process within this long, whatever is stuck.
const SHUTDOWN_LIMIT: Duration = Duration::from_secs(3);

fn main() -> anyhow::Result<ExitCode> {
    let headless = std::env::args().any(|a| a == "--headless");

    let _instance = match single_instance::acquire().context("single-instance check failed")? {
        Instance::First(guard) => guard,
        Instance::AlreadyRunning => {
            eprintln!("RTSP Cam is already running.");
            return Ok(ExitCode::SUCCESS);
        }
    };

    let store = ConfigStore::open_default()?;
    // Load before logging starts so the configured level applies from the first line.
    let loaded = store.load();
    let level = loaded
        .as_ref()
        .map(|c| c.settings.log_level)
        .unwrap_or_default();

    let mut log_opts = LogOptions::new(paths::log_dir()?, "rtspcam");
    log_opts.level = level;
    log_opts.stderr = cfg!(debug_assertions) || headless;
    let _log = logging::init(&log_opts).context("could not start logging")?;

    let config = loaded.with_context(|| format!("could not load {}", store.path().display()))?;
    tracing::info!(
        path = %store.path().display(),
        streams = config.streams.len(),
        "config loaded"
    );
    for issue in config.validate() {
        tracing::warn!(%issue, "invalid stream configuration");
    }

    if !headless {
        eprintln!("The window is not built yet; run with --headless.");
        return Ok(ExitCode::FAILURE);
    }
    run_headless(&store, config)?;
    Ok(ExitCode::SUCCESS)
}

fn run_headless(store: &ConfigStore, config: rtspcam_core::Config) -> anyhow::Result<()> {
    let manager = CameraManager::start(Arc::new(VcamBackend::start()), ManagerOptions::default())?;
    manager.apply(&config);

    // Hand-edits to config.json take effect live. Invalid JSON keeps the last good config.
    let manager = Arc::new(manager);
    let watcher = {
        let manager = manager.clone();
        store.watch(move |result| match result {
            Ok(config) => {
                tracing::info!(streams = config.streams.len(), "config changed on disk");
                manager.apply(&config);
            }
            Err(e) => tracing::error!(error = %e, "config.json is invalid; keeping the old one"),
        })?
    };

    tracing::info!("running; press Ctrl+C to quit");
    manager
        .handle()
        .block_on(tokio::signal::ctrl_c())
        .context("could not wait for Ctrl+C")?;
    tracing::info!("quitting");

    drop(watcher);
    match Arc::try_unwrap(manager) {
        Ok(manager) => manager.shutdown(SHUTDOWN_LIMIT),
        Err(_) => tracing::warn!("the manager is still shared; ending the process instead"),
    }
    Ok(())
}
