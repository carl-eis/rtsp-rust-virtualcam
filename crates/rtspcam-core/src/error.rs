//! Error types.

use std::io;
use std::path::PathBuf;

/// Errors from encrypting or decrypting a [`Secret`](crate::Secret).
#[derive(Debug, thiserror::Error)]
pub enum SecretError {
    #[error("DPAPI {op} failed: {source}")]
    Dpapi {
        op: &'static str,
        #[source]
        source: io::Error,
    },
    #[error("stored secret is not valid base64")]
    Base64(#[from] base64::DecodeError),
    #[error("decrypted secret is not valid UTF-8")]
    Utf8,
    #[error("no secret store is set up in this program")]
    NoStore,
    /// The store could not encrypt or decrypt the value; the text says why.
    #[error("{0}")]
    Unreadable(String),
}

/// Errors from loading, saving or watching the configuration file.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("could not determine the user's application data folder")]
    NoAppDataDir,
    #[error("I/O error on {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("{path} is not valid config JSON: {source}")]
    Parse {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error("could not serialize the config: {0}")]
    Serialize(#[source] serde_json::Error),
    #[error(
        "config schema version {found} is newer than this build supports ({supported}); \
         update RTSP Cam or restore config.json.bak"
    )]
    UnsupportedVersion { found: u64, supported: u64 },
    #[error("config migration from version {from} failed: {reason}")]
    Migration { from: u64, reason: String },
    #[cfg(feature = "watch")]
    #[error("could not watch the config file: {0}")]
    Watch(#[from] notify::Error),
}

impl ConfigError {
    pub(crate) fn io(path: impl Into<PathBuf>, source: io::Error) -> Self {
        Self::Io {
            path: path.into(),
            source,
        }
    }
}
