//! What a pipeline is doing, for the UI, logs and the camera manager.

use std::time::{Duration, Instant};

use crate::{PipelineError, VideoCodec};

/// The state of one stream's pipeline.
#[derive(Debug, Clone, PartialEq)]
pub enum StreamState {
    /// Not running.
    Idle,
    /// Opening the RTSP session (attempt 1, 2, ...), or waiting for the first picture.
    Connecting { attempt: u32 },
    /// Pictures are being decoded.
    Streaming(StreamStats),
    /// The last attempt failed; the next one starts at `retry_at`.
    Retrying {
        error: PipelineError,
        retry_at: Instant,
        attempt: u32,
    },
}

impl StreamState {
    pub fn is_streaming(&self) -> bool {
        matches!(self, Self::Streaming(_))
    }

    /// One-line summary, for example "Streaming H.264 1280x720 30.0 fps".
    pub fn summary(&self) -> String {
        match self {
            Self::Idle => "Idle".to_owned(),
            Self::Connecting { attempt: 1 } => "Connecting".to_owned(),
            Self::Connecting { attempt } => format!("Connecting (attempt {attempt})"),
            Self::Streaming(s) => format!(
                "Streaming {} {}x{} {:.1} fps",
                s.codec, s.width, s.height, s.fps
            ),
            Self::Retrying {
                error, retry_at, ..
            } => format!(
                "Error: {error} (retrying in {} s)",
                retry_at
                    .saturating_duration_since(Instant::now())
                    .as_secs_f32()
                    .ceil()
            ),
        }
    }
}

/// Measurements over the last second of streaming.
#[derive(Debug, Clone, PartialEq)]
pub struct StreamStats {
    pub codec: VideoCodec,
    pub decoder: String,
    /// Decoded picture size.
    pub width: u32,
    pub height: u32,
    /// Decoded pictures per second.
    pub fps: f32,
    /// Received video bits per second.
    pub bitrate: u64,
    /// Average time from a frame's last packet arriving to its picture being decoded.
    pub latency: Duration,
    /// Totals for this session.
    pub frames_decoded: u64,
    pub decode_errors: u64,
    pub packets_lost: u64,
}
