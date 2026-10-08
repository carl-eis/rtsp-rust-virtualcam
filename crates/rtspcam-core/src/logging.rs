//! `tracing` setup: daily rolling log files, optional stderr output, a runtime-adjustable
//! level, and a panic hook that logs panics before the default handler runs.

use std::backtrace::Backtrace;
use std::fmt;
use std::panic::{self, PanicHookInfo};
use std::path::PathBuf;

use tracing_appender::non_blocking::WorkerGuard;
use tracing_appender::rolling::{self, Rotation};
use tracing_subscriber::layer::SubscriberExt as _;
use tracing_subscriber::util::SubscriberInitExt as _;
use tracing_subscriber::{EnvFilter, Registry, fmt as tfmt, reload};

use crate::config::LogLevel;

/// Environment variable that overrides the configured level, using `EnvFilter` syntax
/// (for example `RTSPCAM_LOG=debug,retina=trace`).
pub const LOG_ENV_VAR: &str = "RTSPCAM_LOG";

#[derive(Debug, Clone)]
pub struct LogOptions {
    /// Folder for the log files, usually [`paths::log_dir`](crate::paths::log_dir).
    pub dir: PathBuf,
    /// File name prefix, for example `"rtspcam"` gives `rtspcam.2026-10-08.log`.
    pub file_prefix: String,
    pub level: LogLevel,
    /// Also write to stderr (for the CLI and debug builds).
    pub stderr: bool,
    /// Number of daily files to keep.
    pub max_files: usize,
}

impl LogOptions {
    pub fn new(dir: impl Into<PathBuf>, file_prefix: impl Into<String>) -> Self {
        Self {
            dir: dir.into(),
            file_prefix: file_prefix.into(),
            level: LogLevel::Info,
            stderr: false,
            max_files: 7,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum LoggingError {
    #[error("could not create the log file in {dir}: {source}")]
    Appender {
        dir: PathBuf,
        #[source]
        source: rolling::InitError,
    },
    #[error("a global tracing subscriber is already installed")]
    AlreadyInitialized(#[from] tracing_subscriber::util::TryInitError),
}

/// Keeps the background log writer alive. Dropping it flushes buffered lines, so keep it in
/// `main` until the process exits.
pub struct LogGuard {
    _worker: WorkerGuard,
    filter: reload::Handle<EnvFilter, Registry>,
    env_override: bool,
}

impl fmt::Debug for LogGuard {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LogGuard").field("env_override", &self.env_override).finish_non_exhaustive()
    }
}

impl LogGuard {
    /// Changes the log level at runtime (for example after the user changes it in Settings).
    /// Ignored when [`LOG_ENV_VAR`] is set.
    pub fn set_level(&self, level: LogLevel) {
        if self.env_override {
            return;
        }
        if let Err(e) = self.filter.reload(EnvFilter::new(level.as_str())) {
            tracing::warn!(error = %e, "could not change the log level");
        }
    }
}

/// Installs the global subscriber and the [panic hook](install_panic_hook).
pub fn init(opts: &LogOptions) -> Result<LogGuard, LoggingError> {
    let appender = rolling::Builder::new()
        .rotation(Rotation::DAILY)
        .filename_prefix(&opts.file_prefix)
        .filename_suffix("log")
        .max_log_files(opts.max_files)
        .build(&opts.dir)
        .map_err(|source| LoggingError::Appender { dir: opts.dir.clone(), source })?;
    let (writer, worker) = tracing_appender::non_blocking(appender);

    let env_filter = std::env::var(LOG_ENV_VAR).ok().and_then(|v| EnvFilter::try_new(v).ok());
    let env_override = env_filter.is_some();
    let filter = env_filter.unwrap_or_else(|| EnvFilter::new(opts.level.as_str()));
    let (filter, handle) = reload::Layer::new(filter);

    let file_layer = tfmt::layer().with_writer(writer).with_ansi(false).with_thread_names(true);
    let stderr_layer = opts.stderr.then(|| tfmt::layer().with_writer(std::io::stderr));

    tracing_subscriber::registry().with(filter).with(file_layer).with(stderr_layer).try_init()?;
    install_panic_hook();

    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        dir = %opts.dir.display(),
        "logging started"
    );
    Ok(LogGuard { _worker: worker, filter: handle, env_override })
}

/// Logs every panic (message, location, thread and backtrace) at `error` level, then runs the
/// previously installed hook.
pub fn install_panic_hook() {
    let previous = panic::take_hook();
    panic::set_hook(Box::new(move |info| {
        log_panic(info);
        previous(info);
    }));
}

fn log_panic(info: &PanicHookInfo<'_>) {
    let payload = info.payload();
    let message = payload
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| payload.downcast_ref::<String>().map(String::as_str))
        .unwrap_or("<non-string panic payload>");
    let location = info.location().map(ToString::to_string).unwrap_or_default();
    let thread = std::thread::current();
    let thread = thread.name().unwrap_or("<unnamed>");
    let backtrace = Backtrace::force_capture();
    tracing::error!(target: "panic", %location, thread, "panic: {message}\n{backtrace}");
}
