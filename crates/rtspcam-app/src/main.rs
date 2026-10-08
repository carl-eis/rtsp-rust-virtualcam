//! `rtspcam.exe`.
//!
//! Phase 1 only wires up logging and config loading. The camera manager (Phase 4) and the
//! native UI and tray icon (Phase 5) build on this.

use anyhow::Context as _;
use rtspcam_core::config::ConfigStore;
use rtspcam_core::logging::{self, LogOptions};
use rtspcam_core::paths;

fn main() -> anyhow::Result<()> {
    let store = ConfigStore::open_default()?;
    // Load before logging starts so the configured level applies from the first line.
    let loaded = store.load();
    let level = loaded.as_ref().map(|c| c.settings.log_level).unwrap_or_default();

    let mut log_opts = LogOptions::new(paths::log_dir()?, "rtspcam");
    log_opts.level = level;
    log_opts.stderr = cfg!(debug_assertions);
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
    for stream in config.streams.iter().filter(|s| s.enabled) {
        tracing::info!(id = %stream.id, name = %stream.name, url = %stream.url(), "stream");
    }
    Ok(())
}
