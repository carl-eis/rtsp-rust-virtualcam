//! The app's engine: everything `rtspcam.exe` does except drawing windows.
//!
//! - [`manager`]: the [`CameraManager`](manager::CameraManager), which keeps one pipeline, one
//!   pipe server and one virtual camera per enabled stream in step with the configuration.
//! - [`backend`]: creates and removes the virtual cameras (a fake one stands in for tests).
//! - `overlay`: the name and time drawn onto pictures.
//! - [`source`]: serves a camera's frames to the media source DLL.
//! - [`status`]: what the UI shows for each camera.
//! - [`single_instance`]: one `rtspcam.exe` per user session.

pub mod autostart;
pub mod backend;
pub mod form;
pub mod manager;
pub(crate) mod overlay;
pub mod single_instance;
pub mod source;
pub mod status;
pub mod ui;

pub use manager::{CameraManager, ManagerOptions, Preview};
pub use status::{Activity, CameraStatus, VcamState};
