//! The app's engine: everything `rtspcam.exe` does except drawing windows.
//!
//! - [`manager`]: the [`CameraManager`](manager::CameraManager), which keeps one pipeline and
//!   one virtual camera per enabled stream in step with the configuration.
//! - `overlay`: the name and time drawn onto pictures.
//! - [`source`]: a camera's frames, as its virtual camera backend asks for them.
//! - [`status`]: what the UI shows for each camera.
//!
//! What differs per OS (virtual cameras, autostart, single instance, ...) comes from
//! `rtspcam-platform`.

pub mod form;
pub mod manager;
pub(crate) mod overlay;
pub mod source;
pub mod status;
pub mod ui;

pub use manager::{CameraManager, ManagerOptions, Preview};
pub use status::{Activity, CameraStatus, VcamState};
