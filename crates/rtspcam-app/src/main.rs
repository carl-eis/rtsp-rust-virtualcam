//! `rtspcam.exe`.
//!
//! Opens the window (and tray icon) and runs the cameras until the user quits. `--headless`
//! runs the cameras without a window, for services-style use and for tests. `--minimized`
//! starts hidden in the tray (or minimized, if "Minimize to tray" is off); it is what the
//! "Start with Windows" entry passes.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context as _;
use rtspcam_app::backend::VcamBackend;
use rtspcam_app::single_instance::{self, Instance};
use rtspcam_app::{CameraManager, ManagerOptions, ui};
use rtspcam_core::config::ConfigStore;
use rtspcam_core::constants::APP_DISPLAY_NAME;
use rtspcam_core::logging::{self, LogOptions};
use rtspcam_core::paths;
use windows::Win32::System::Console::{ATTACH_PARENT_PROCESS, AttachConsole};
use windows::Win32::UI::WindowsAndMessaging::{MB_ICONERROR, MB_OK, MessageBoxW};
use windows::core::HSTRING;

/// Quitting must end the process within this long, whatever is stuck.
const SHUTDOWN_LIMIT: Duration = Duration::from_secs(3);

fn main() -> ExitCode {
    // Started from a terminal, a windowed build can still be stopped with Ctrl+C.
    // SAFETY: fails harmlessly when there is no parent console.
    unsafe {
        let _ = AttachConsole(ATTACH_PARENT_PROCESS);
    }
    match run() {
        Ok(code) => code,
        Err(e) => {
            tracing::error!(error = %format!("{e:#}"), "fatal error");
            eprintln!("{APP_DISPLAY_NAME}: {e:#}");
            // A windowed app has no console to print to.
            // SAFETY: a plain message box with owned strings.
            unsafe {
                MessageBoxW(
                    None,
                    &HSTRING::from(format!("{e:#}")),
                    &HSTRING::from(APP_DISPLAY_NAME),
                    MB_OK | MB_ICONERROR,
                );
            }
            ExitCode::FAILURE
        }
    }
}

fn run() -> anyhow::Result<ExitCode> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let has = |flag: &str| args.iter().any(|a| a == flag);
    let (headless, minimized) = (has("--headless"), has("--minimized"));

    if !rtspcam_vcam_mgr::is_supported() {
        anyhow::bail!(
            "{APP_DISPLAY_NAME} requires Windows 11 (virtual cameras are not available)."
        );
    }

    let instance = match single_instance::acquire().context("single-instance check failed")? {
        Instance::First(guard) => guard,
        Instance::AlreadyRunning => return Ok(ExitCode::SUCCESS),
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

    let manager = CameraManager::start(Arc::new(VcamBackend::start()), ManagerOptions::default())?;
    if headless {
        run_headless(&store, &config, manager)?;
        return Ok(ExitCode::SUCCESS);
    }
    let code = ui::run(store, config, manager, instance, minimized)?;
    Ok(if code == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}

fn run_headless(
    store: &ConfigStore,
    config: &rtspcam_core::Config,
    manager: CameraManager,
) -> anyhow::Result<()> {
    manager.apply(config);

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
