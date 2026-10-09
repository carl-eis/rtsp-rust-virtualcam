//! Virtual cameras: what the camera manager needs from the OS.

use std::fmt;
use std::sync::Arc;

use rtspcam_ipc::FrameSource;
use tokio::runtime::Handle;
use uuid::Uuid;

/// What a camera should look like to apps.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CameraSpec {
    /// The stream id. It names the camera's endpoint, so it must stay stable.
    pub id: Uuid,
    /// Shown in camera pickers (Windows adds " – Windows Virtual Camera" in places).
    pub name: String,
    /// The stream's configured output (width, height, fps), listed first among the formats.
    pub preferred: (u32, u32, u32),
}

/// Why a camera could not be made.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CameraError {
    /// This OS has no virtual camera backend yet. A status to show, not a failure.
    Unsupported(String),
    /// The backend exists but this camera could not be made (not installed, access denied,
    /// an OS too old, ...). The text is shown to the user.
    Failed(String),
}

impl CameraError {
    pub fn message(&self) -> &str {
        match self {
            Self::Unsupported(m) | Self::Failed(m) => m,
        }
    }
}

impl fmt::Display for CameraError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.message())
    }
}

impl std::error::Error for CameraError {}

/// Makes virtual cameras and gets frames to them.
///
/// How each OS implements it:
///
/// - **Windows** (`windows::VcamBackend`): `MFCreateVirtualCamera` with session lifetime, so the
///   cameras vanish with the process. Windows' Frame Server loads `rtspcam_vcam.dll`, which
///   connects to the named pipe `\\.\pipe\rtspcam\<id>`; `create` serves `frames` on it.
/// - **Linux** (future): v4l2loopback. `create` opens the camera's `/dev/videoN` and starts a
///   thread that calls `frames.client_connected` once and then `frames.next_frame` at the
///   camera's rate, writing each picture to the device (push; there is no consumer process).
/// - **macOS** (future): a CoreMediaIO Camera Extension. `create` asks the extension for a
///   device and serves `frames` to it over a socket or XPC (like Windows).
/// - **Linux and macOS today**: [`UnsupportedCameras`].
///
/// Methods block (creating a Windows camera takes about a second). The camera manager calls
/// them on its blocking thread pool, never on the UI thread.
pub trait VirtualCameraBackend: Send + Sync + 'static {
    /// Whether this computer can have virtual cameras at all. [`CameraError::Unsupported`] on an
    /// OS without a backend; [`CameraError::Failed`] when the backend exists but this computer
    /// lacks something (Windows 10).
    fn check(&self) -> Result<(), CameraError>;

    /// Creates (or re-creates) the camera and starts delivering `frames` to it. `runtime` is
    /// the camera manager's tokio runtime, for backends that serve frames asynchronously.
    ///
    /// Frame delivery may continue after an error (Windows keeps serving the pipe so the camera
    /// recovers if it is created later); [`remove`](Self::remove) stops it either way.
    fn create(
        &self,
        spec: &CameraSpec,
        frames: Arc<dyn FrameSource>,
        runtime: &Handle,
    ) -> Result<(), CameraError>;

    /// Removes one camera and stops its frame delivery. Unknown ids are ignored.
    fn remove(&self, id: Uuid);

    /// Removes every camera, stops all frame delivery and releases what the backend holds.
    fn remove_all(&self);
}

/// The backend for an OS without virtual camera support yet: every camera is
/// [`CameraError::Unsupported`]. The rest of the app (preview, discovery, config) still works.
#[derive(Debug, Default, Clone, Copy)]
pub struct UnsupportedCameras;

/// The status shown for each stream on an OS without a camera backend.
pub const UNSUPPORTED_MESSAGE: &str = "Virtual cameras are not supported on this platform yet";

impl VirtualCameraBackend for UnsupportedCameras {
    fn check(&self) -> Result<(), CameraError> {
        Err(CameraError::Unsupported(UNSUPPORTED_MESSAGE.to_owned()))
    }

    fn create(
        &self,
        _spec: &CameraSpec,
        _frames: Arc<dyn FrameSource>,
        _runtime: &Handle,
    ) -> Result<(), CameraError> {
        Err(CameraError::Unsupported(UNSUPPORTED_MESSAGE.to_owned()))
    }

    fn remove(&self, _id: Uuid) {}

    fn remove_all(&self) {}
}
