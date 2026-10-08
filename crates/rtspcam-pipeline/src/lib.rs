//! RTSP ingest (`retina`), decoding (Media Foundation / openh264), scaling and the per-camera
//! latest-frame bus.
//!
//! - [`source`]: connects to an RTSP server and yields encoded video frames.
//! - [`decode`]: Media Foundation and OpenH264 decoders.
//! - [`frame`]: decoded NV12 pictures.
//! - [`bus`]: the per-camera "latest frame" channel between the decoder and its readers.
//! - [`scale`]: NV12 scaling with the configured fit mode, and NV12 → BGRA for the preview.
//! - [`error`]: errors classified by what the user can do about them.

pub mod bus;
pub mod decode;
pub mod error;
pub mod frame;
pub mod scale;
pub mod source;

pub use bus::{FrameBus, FrameReceiver};
pub use error::{ErrorKind, PipelineError};
pub use frame::Frame;
pub use scale::{Matrix, Rect, Scaler};
pub use source::{Credentials, EncodedFrame, RtspSource, SourceOptions, StreamInfo, VideoCodec};
