//! Shared building blocks for RTSP Cam.
//!
//! - [`config`]: the JSON configuration model (`%APPDATA%\RtspCam\config.json`), validation,
//!   atomic persistence, schema migrations and (with the `watch` feature) file watching.
//! - [`secret`]: DPAPI-protected strings used for stream passwords.
//! - [`constants`]: names and identifiers shared by the app and the virtual camera DLL.
//! - [`logging`] (feature `logging`): rolling-file `tracing` setup and a panic hook.
//!
//! This crate is also used by the virtual camera DLL, which runs inside a Windows service.
//! The DLL depends on it with `default-features = false` so it doesn't pull in `notify`
//! or the `tracing` subscriber stack.

pub mod config;
pub mod constants;
pub mod error;
#[cfg(feature = "logging")]
pub mod logging;
pub mod paths;
pub mod secret;

pub use config::{
    AppSettings, Config, FitMode, LogLevel, OutputFormat, Protocol, StreamConfig, Transport,
};
pub use error::{ConfigError, SecretError};
pub use secret::Secret;
