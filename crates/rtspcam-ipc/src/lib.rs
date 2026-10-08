//! Frame and control protocol between `rtspcam.exe` and `rtspcam_vcam.dll`.
//!
//! Phase 3 adds the frame header + NV12 payload format and the pipe/shared-memory client and
//! server. The client side is used inside the Frame Server service and must not depend on tokio.

pub use rtspcam_core::constants::{PIPE_PREFIX, frame_pipe_name};
