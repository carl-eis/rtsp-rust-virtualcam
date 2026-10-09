//! RTSP Cam's engine: everything the app does except drawing windows and talking to the OS.
//!
//! - [`manager`]: the [`CameraManager`], which keeps one pipeline and one virtual camera per
//!   enabled stream in step with the configuration, and hands out [`Preview`]s.
//! - [`source`]: a camera's frames, as its virtual camera backend asks for them.
//! - [`status`]: what the UI shows for each camera.
//! - [`form`]: the Add/Edit stream form as plain data, with validation.
//! - [`probe`]: "Test connection".
//! - [`discovery`]: helpers for "Find cameras".
//! - `overlay`: the name and time drawn onto pictures.
//!
//! Nothing here is OS-specific. The OS services come in as trait objects from
//! `rtspcam-platform` (for example the [`VirtualCameraBackend`](rtspcam_platform::VirtualCameraBackend)
//! given to [`CameraManager::start`]), so the engine's tests run everywhere with fakes.

pub mod discovery;
pub mod form;
pub mod manager;
pub(crate) mod overlay;
pub mod probe;
pub mod source;
pub mod status;

pub use manager::{CameraManager, ManagerOptions, Preview};
pub use status::{Activity, CameraStatus, VcamState};
