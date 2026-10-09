//! Frame and control protocol between the RTSP Cam app and a virtual camera that pulls frames
//! from it (on Windows, `rtspcam_vcam.dll` inside Frame Server).
//!
//! When a consumer (Discord, the Camera app, ...) starts the camera, the camera side connects,
//! says which format the consumer picked, and receives frames already scaled and converted to
//! that format. How the two sides reach each other (a named pipe on Windows, a Unix socket
//! elsewhere) is up to the transport in `rtspcam-platform`; nothing here is OS-specific.
//!
//! - [`protocol`]: the message format, shared by both sides.
//! - [`source`]: [`FrameSource`], where a camera's frames come from in the app.
//! - [`client`]: blocking client for the camera side (no tokio: the DLL runs inside a Windows
//!   service).
//! - [`server`] (feature `server`): async server for the app, over any [`server::FrameListener`].

pub mod client;
pub mod protocol;
#[cfg(feature = "server")]
pub mod server;
pub mod source;

pub use protocol::{FrameHeader, Message, PixelFormat, ProtocolError, StreamStatus, VideoFormat};
pub use rtspcam_core::constants::{PIPE_PREFIX, frame_pipe_name};
pub use source::FrameSource;
