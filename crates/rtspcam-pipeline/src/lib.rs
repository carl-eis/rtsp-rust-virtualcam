//! RTSP ingest (`retina`), decoding (Media Foundation / openh264), scaling and the per-camera
//! latest-frame bus.
//!
//! - [`frame`]: decoded NV12 pictures.
//! - [`bus`]: the per-camera "latest frame" channel between the decoder and its readers.
//! - [`scale`]: NV12 scaling with the configured fit mode, and NV12 → BGRA for the preview.

pub mod bus;
pub mod frame;
pub mod scale;

pub use bus::{FrameBus, FrameReceiver};
pub use frame::Frame;
pub use scale::{Matrix, Rect, Scaler};
