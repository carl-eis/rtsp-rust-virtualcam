//! RTSP Cam's native boundary: everything that differs between Windows, Linux and macOS.
//!
//! The rest of the workspace is portable and talks to the OS only through the traits listed
//! here. This is the only crate (apart from the Windows-only DLL and its manager) with
//! `cfg(target_os)` code; each trait's docs say how each OS implements it.
//!
//! | Trait | Windows | Linux | macOS |
//! |---|---|---|---|
//! | [`VirtualCameraBackend`] | Media Foundation virtual cameras | v4l2loopback | not yet ([`UnsupportedCameras`]) |
//! | [`SecretStore`] | DPAPI | key in Secret Service | key in Keychain |
//! | [`Autostart`] | HKCU `Run` key | XDG autostart entry | LaunchAgent |
//! | [`SingleInstance`] | named mutex + event | lock file + Unix socket | lock file + Unix socket |
//! | [`FrameTransport`] | named pipes | Unix sockets | Unix sockets |
//! | [`FileReplace`] | `ReplaceFileW` | copy, then rename | copy, then rename |
//! | [`PlatformDecoders`] | Media Foundation | none (OpenH264) | none (OpenH264) |
//!
//! [`SecretStore`], [`FileReplace`] and [`PlatformDecoders`] are defined next to the code that
//! calls them (`rtspcam-core`, `rtspcam-pipeline`) and re-exported here.
//!
//! A program calls [`install`] once at startup, then the constructors below for the services
//! it needs, and passes them on as trait objects.
//!
//! [`push`] holds the portable frame loop ([`push::Pusher`]) that push camera backends share.

pub mod autostart;
pub mod camera;
pub mod desktop;
pub mod instance;
mod login_items;
pub mod push;
pub mod transport;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(unix)]
mod unix;
#[cfg(windows)]
mod windows;

#[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
compile_error!("rtspcam-platform supports Windows, Linux and macOS");

#[cfg(target_os = "linux")]
use self::linux as os;
#[cfg(target_os = "macos")]
use self::macos as os;
#[cfg(windows)]
use self::windows as os;

use std::sync::Arc;

pub use autostart::{Autostart, MINIMIZED_ARG};
pub use camera::{
    CameraError, CameraSpec, UNSUPPORTED_MESSAGE, UnsupportedCameras, VirtualCameraBackend,
};
pub use instance::{Instance, InstanceLock, SingleInstance};
pub use rtspcam_core::config::{CopyThenRename, FileReplace};
pub use rtspcam_core::secret::SecretStore;
pub use rtspcam_pipeline::decode::PlatformDecoders;
pub use transport::{FrameTransport, ReadWrite};

/// Sets up the process-wide hooks: the [`SecretStore`] that encrypts passwords in the config,
/// and the OS's video decoders. Call once at startup, before loading the config. Calling it
/// again does nothing.
pub fn install() {
    os::install();
}

/// This OS's virtual camera backend. On Windows this starts the thread that owns Media
/// Foundation; create one per process.
pub fn camera_backend() -> Arc<dyn VirtualCameraBackend> {
    os::camera_backend()
}

/// The [`SecretStore`] [`install`] uses, for programs that want it directly.
pub fn secret_store() -> Box<dyn SecretStore> {
    os::secret_store()
}

/// This OS's "start at login" entry.
pub fn autostart() -> Box<dyn Autostart> {
    os::autostart()
}

/// One running copy per user, under `name` (the app uses `"RtspCam"`).
pub fn single_instance(name: &str) -> Box<dyn SingleInstance> {
    os::single_instance(name)
}

/// How to put a saved config file in place; pass it to `ConfigStore::with_replacer`.
pub fn file_replacer() -> Arc<dyn FileReplace> {
    os::file_replacer()
}

/// This OS's per-camera frame endpoints.
pub fn frame_transport() -> Arc<dyn FrameTransport> {
    os::frame_transport()
}
