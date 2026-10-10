//! Linux: v4l2loopback virtual cameras, secrets in Secret Service, XDG autostart, Unix sockets.

mod camera;
mod v4l2;

use std::io;
use std::path::Path;
use std::sync::Arc;

use rtspcam_core::config::{CopyThenRename, FileReplace};

use crate::login_items::XdgAutostart;
use crate::unix::instance::LockFile;
use crate::unix::secrets::{KeyFile, Keyring, SealedStore};
pub(crate) use crate::unix::termination_requested;
use crate::unix::transport::UnixSockets;
use crate::{Autostart, FrameTransport, SecretStore, SingleInstance, VirtualCameraBackend};

pub(crate) fn install() {
    rtspcam_core::secret::install_store(secret_store());
}

/// v4l2loopback devices. Without the module every camera is unsupported, with a status that
/// says what to install.
pub(crate) fn camera_backend() -> Arc<dyn VirtualCameraBackend> {
    Arc::new(camera::V4l2Loopback::new())
}

fn open_keyring() -> keyring_core::Result<Arc<keyring_core::CredentialStore>> {
    Ok(zbus_secret_service_keyring_store::Store::new()?)
}

pub(crate) fn secret_store() -> Box<dyn SecretStore> {
    let key_file = rtspcam_core::paths::config_dir()
        .map(|d| d.join("secret.key"))
        .unwrap_or_else(|_| std::env::temp_dir().join("rtspcam-secret.key"));
    Box::new(SealedStore::new(
        Box::new(Keyring::new(open_keyring)),
        Box::new(KeyFile { path: key_file }),
    ))
}

pub(crate) fn autostart() -> Box<dyn Autostart> {
    let dir = directories::BaseDirs::new()
        .map(|d| d.config_dir().join("autostart"))
        .unwrap_or_else(|| Path::new(".config/autostart").to_owned());
    Box::new(XdgAutostart::in_dir(&dir))
}

pub(crate) fn single_instance(name: &str) -> Box<dyn SingleInstance> {
    Box::new(LockFile::new(name))
}

pub(crate) fn file_replacer() -> Arc<dyn FileReplace> {
    Arc::new(CopyThenRename)
}

pub(crate) fn frame_transport() -> Arc<dyn FrameTransport> {
    Arc::new(UnixSockets::new())
}

pub(crate) fn open_folder(folder: &Path) -> io::Result<()> {
    std::process::Command::new("xdg-open")
        .arg(folder)
        .spawn()
        .map(drop)
}

pub(crate) fn attach_parent_console() {}

/// (resident set, anonymous resident memory) from `/proc/self/status`.
pub(crate) fn process_memory() -> Option<(usize, usize)> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    let kib = |key: &str| -> Option<usize> {
        let line = status.lines().find(|l| l.starts_with(key))?;
        let value: usize = line[key.len()..]
            .trim()
            .trim_end_matches("kB")
            .trim()
            .parse()
            .ok()?;
        Some(value * 1024)
    };
    Some((kib("VmRSS:")?, kib("RssAnon:").unwrap_or(0)))
}
