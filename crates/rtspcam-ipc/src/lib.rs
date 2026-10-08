//! Frame and control protocol between `rtspcam.exe` and `rtspcam_vcam.dll`.
//!
//! The app serves one named pipe per camera (`\.\pipe\rtspcam\<stream id>`). When a consumer
//! (Discord, the Camera app, ...) starts the camera, the DLL connects, says which format the
//! consumer picked, and receives frames already scaled and converted to that format.
//!
//! - [`protocol`]: the message format, shared by both sides.
//! - [`client`]: blocking client for the DLL (no tokio: it runs inside a Windows service).
//! - [`server`] (feature `server`): async server for the app.

pub mod protocol;

#[cfg(windows)]
pub mod client;
#[cfg(all(windows, feature = "server"))]
pub mod server;

pub use protocol::{FrameHeader, Message, PixelFormat, ProtocolError, StreamStatus, VideoFormat};
pub use rtspcam_core::constants::{PIPE_PREFIX, frame_pipe_name};
