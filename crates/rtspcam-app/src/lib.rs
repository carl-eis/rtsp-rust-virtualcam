//! The app's engine: everything `rtspcam.exe` does except drawing windows.
//!
//! - [`manager`]: the [`CameraManager`](manager::CameraManager), which keeps one pipeline, one
//!   pipe server and one virtual camera per enabled stream in step with the configuration.
//! - [`backend`]: creates and removes the virtual cameras (a fake one stands in for tests).
//! - [`source`]: serves a camera's frames to the media source DLL.
//! - [`status`]: what the UI shows for each camera.
//! - [`single_instance`]: one `rtspcam.exe` per user session.

pub mod backend;
pub mod manager;
pub mod single_instance;
pub mod source;
pub mod status;

pub use manager::{CameraManager, ManagerOptions, Preview};
pub use status::{Activity, CameraStatus, VcamState};
