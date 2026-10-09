//! RTSP ingest (`retina`), decoding (OpenH264, or the platform's own decoders), scaling and the
//! per-camera latest-frame bus.
//!
//! - [`pipeline`]: a running stream: ingest, decode thread, reconnects and status.
//! - [`source`]: connects to an RTSP server and yields encoded video frames.
//! - [`decode`]: the OpenH264 decoder and the hook for the platform's own decoders.
//! - [`frame`]: decoded NV12 pictures.
//! - [`bus`]: the per-camera "latest frame" channel between the decoder and its readers.
//! - [`scale`]: NV12 scaling with the configured fit mode, and NV12 → BGRA for the preview.
//! - [`error`]: errors classified by what the user can do about them.
//! - [`status`]: stream state and statistics.
//! - [`transform`]: crop, rotate and flip.
//! - [`pattern`]: an animated test pattern.

pub mod bus;
pub mod decode;
pub mod error;
pub mod frame;
pub mod pattern;
pub mod pipeline;
pub mod scale;
pub mod source;
pub mod status;
pub mod transform;

pub use bus::{FrameBus, FrameReceiver};
pub use error::{ErrorKind, PipelineError};
pub use frame::Frame;
pub use pipeline::{BackoffConfig, Pipeline, PipelineOptions};
pub use scale::{Matrix, Rect, Scaler};
pub use source::{Credentials, EncodedFrame, RtspSource, SourceOptions, StreamInfo, VideoCodec};
pub use status::{StreamState, StreamStats};
