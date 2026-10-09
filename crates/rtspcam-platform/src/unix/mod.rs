//! Shared by Linux and macOS.
//!
//! Placeholder until the Linux and macOS services land (step C of
//! `documentation/09-cross-platform-plan.md`): no secret store, no autostart, no transport,
//! and every copy of the app is "the first".

use std::fmt;
use std::io;
use std::path::Path;
use std::sync::Arc;

use rtspcam_core::SecretError;
use rtspcam_core::config::{CopyThenRename, FileReplace};
use rtspcam_ipc::server::FrameListener;
use uuid::Uuid;
use zeroize::Zeroizing;

use crate::camera::UnsupportedCameras;
use crate::instance::{Instance, InstanceLock};
use crate::transport::ReadWrite;
use crate::{Autostart, FrameTransport, SecretStore, SingleInstance, VirtualCameraBackend};

fn not_yet() -> io::Error {
    io::Error::new(
        io::ErrorKind::Unsupported,
        "not available on this platform yet",
    )
}

struct NoSecrets;

impl SecretStore for NoSecrets {
    fn seal(&self, _plain: &str) -> Result<String, SecretError> {
        Err(SecretError::Unreadable(not_yet().to_string()))
    }

    fn unseal(&self, _stored: &str) -> Result<Zeroizing<String>, SecretError> {
        Err(SecretError::Unreadable(not_yet().to_string()))
    }
}

struct NoAutostart;

impl Autostart for NoAutostart {
    fn label(&self) -> &'static str {
        "Start at login"
    }

    fn is_enabled(&self) -> bool {
        false
    }

    fn set(&self, _enabled: bool) -> io::Result<()> {
        Err(not_yet())
    }
}

struct AlwaysFirst;

#[derive(Debug)]
struct NoLock;

impl InstanceLock for NoLock {
    fn on_show(&mut self, _on_show: Box<dyn Fn() + Send>) {}
}

impl SingleInstance for AlwaysFirst {
    fn acquire(&self) -> io::Result<Instance> {
        Ok(Instance::First(Box::new(NoLock)))
    }
}

struct NoTransport;

impl FrameTransport for NoTransport {
    fn listen(&self, _id: Uuid) -> io::Result<Box<dyn FrameListener>> {
        Err(not_yet())
    }

    fn connect(&self, _id: Uuid) -> io::Result<Box<dyn ReadWrite>> {
        Err(not_yet())
    }

    fn is_served(&self, _id: Uuid) -> bool {
        false
    }
}

impl fmt::Debug for NoTransport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("NoTransport")
    }
}

pub(crate) fn install() {
    rtspcam_core::secret::install_store(secret_store());
}

pub(crate) fn camera_backend() -> Arc<dyn VirtualCameraBackend> {
    Arc::new(UnsupportedCameras)
}

pub(crate) fn secret_store() -> Box<dyn SecretStore> {
    Box::new(NoSecrets)
}

pub(crate) fn autostart() -> Box<dyn Autostart> {
    Box::new(NoAutostart)
}

pub(crate) fn single_instance(_name: &str) -> Box<dyn SingleInstance> {
    Box::new(AlwaysFirst)
}

pub(crate) fn file_replacer() -> Arc<dyn FileReplace> {
    Arc::new(CopyThenRename)
}

pub(crate) fn frame_transport() -> Arc<dyn FrameTransport> {
    Arc::new(NoTransport)
}

pub(crate) fn open_folder(_folder: &Path) -> io::Result<()> {
    Err(not_yet())
}

pub(crate) fn attach_parent_console() {}

pub(crate) fn process_memory() -> Option<(usize, usize)> {
    None
}
