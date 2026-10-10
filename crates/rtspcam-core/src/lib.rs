//! Shared building blocks for RTSP Cam.
//!
//! - [`config`]: the JSON configuration model (`config.json`, see [`paths`]), validation,
//!   atomic persistence, schema migrations and (with the `watch` feature) file watching.
//! - [`secret`]: encrypted strings used for stream passwords, and the hook for the OS store
//!   that encrypts them.
//! - [`constants`]: names and identifiers shared by the app and the virtual camera DLL.
//! - [`logging`] (feature `logging`): rolling-file `tracing` setup and a panic hook.
//!
//! This crate has no OS-specific code; what differs per OS (encrypting secrets, replacing
//! files) is a trait here with implementations in `rtspcam-platform`.
//!
//! It is also used by the Windows virtual camera DLL, which runs inside a Windows service.
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
    AppSettings, Config, FitMode, LogLevel, OutputFormat, Picture, Protocol, StreamConfig, Theme,
    Transport,
};
pub use error::{ConfigError, SecretError};
pub use secret::{Secret, SecretStore};
