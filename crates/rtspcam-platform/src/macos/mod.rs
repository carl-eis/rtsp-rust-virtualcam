//! macOS: no virtual cameras yet; secrets in the login Keychain, a LaunchAgent for autostart,
//! Unix sockets.
//!
//! A CoreMediaIO Camera Extension backend would be a type here implementing
//! [`VirtualCameraBackend`], returned by [`camera_backend`], serving frames to the extension
//! over [`UnixSockets`] (or XPC). Nothing else needs to change.

use std::io;
use std::path::Path;
use std::sync::Arc;

use rtspcam_core::config::{CopyThenRename, FileReplace};

use crate::camera::UnsupportedCameras;
use crate::login_items::LaunchAgent;
use crate::unix::instance::LockFile;
use crate::unix::secrets::{KeyFile, Keyring, SealedStore};
use crate::unix::transport::UnixSockets;
use crate::{Autostart, FrameTransport, SecretStore, SingleInstance, VirtualCameraBackend};

/// The LaunchAgent's label: a reverse-DNS name from the project's home.
const AGENT_LABEL: &str = "io.github.carl-eis.rtspcam";

pub(crate) fn install() {
    rtspcam_core::secret::install_store(secret_store());
}

pub(crate) fn camera_backend() -> Arc<dyn VirtualCameraBackend> {
    Arc::new(UnsupportedCameras)
}

fn open_keyring() -> keyring_core::Result<Arc<keyring_core::CredentialStore>> {
    Ok(apple_native_keyring_store::keychain::Store::new()?)
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
        .map(|d| d.home_dir().join("Library").join("LaunchAgents"))
        .unwrap_or_else(|| Path::new("Library/LaunchAgents").to_owned());
    Box::new(LaunchAgent::in_dir(&dir, AGENT_LABEL))
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
    std::process::Command::new("open")
        .arg(folder)
        .spawn()
        .map(drop)
}

pub(crate) fn attach_parent_console() {}

pub(crate) fn process_memory() -> Option<(usize, usize)> {
    None
}
