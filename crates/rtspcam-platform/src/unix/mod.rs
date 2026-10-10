//! Shared by Linux and macOS: keyring-backed secrets, the lock-file single instance and Unix
//! socket frame transport.

pub(crate) mod instance;
pub(crate) mod secrets;
pub(crate) mod transport;

use std::fs;
use std::io;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};

use rtspcam_core::constants::APP_DIR_NAME;

/// A per-user folder for sockets and lock files, readable only by the user:
/// `$XDG_RUNTIME_DIR/rtspcam` where that exists (Linux), otherwise `$TMPDIR/rtspcam` on macOS
/// (per user there) or `~/.local/share/RtspCam/run` on Linux.
///
/// Kept short: a Unix socket path must fit in about 100 bytes.
pub(crate) fn runtime_dir() -> PathBuf {
    let dirs = directories::BaseDirs::new();
    if let Some(dir) = dirs.as_ref().and_then(directories::BaseDirs::runtime_dir) {
        return dir.join("rtspcam");
    }
    if cfg!(target_os = "macos") {
        return std::env::temp_dir().join("rtspcam");
    }
    match dirs {
        Some(d) => d.data_local_dir().join(APP_DIR_NAME).join("run"),
        None => std::env::temp_dir().join("rtspcam"),
    }
}

/// Creates `dir` (and its parents) and makes it private to the user.
pub(crate) fn private_dir(dir: &Path) -> io::Result<()> {
    fs::create_dir_all(dir)?;
    fs::set_permissions(dir, fs::Permissions::from_mode(0o700))
}

/// The longest socket path the OS takes (`sun_path` is 104 bytes on macOS, 108 on Linux,
/// including the terminating NUL).
const MAX_SOCKET_PATH: usize = 103;

/// Fails clearly when `path` is too long to be a Unix socket.
pub(crate) fn check_socket_path(path: &Path) -> io::Result<()> {
    if path.as_os_str().len() > MAX_SOCKET_PATH {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "the socket path {} is too long for this OS; set XDG_RUNTIME_DIR (or TMPDIR) \
                 to a shorter folder",
                path.display()
            ),
        ));
    }
    Ok(())
}

/// Waits for SIGTERM (logout, shutdown, `kill`, `systemctl stop`) or SIGHUP (the terminal it
/// was started from closed).
pub(crate) async fn termination_requested() -> io::Result<()> {
    use tokio::signal::unix::{SignalKind, signal};

    let mut terminate = signal(SignalKind::terminate())?;
    let mut hangup = signal(SignalKind::hangup())?;
    tokio::select! {
        _ = terminate.recv() => tracing::info!("received SIGTERM"),
        _ = hangup.recv() => tracing::info!("received SIGHUP"),
    }
    Ok(())
}
