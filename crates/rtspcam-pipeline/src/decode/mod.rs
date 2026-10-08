//! Video decoders: compressed [`EncodedFrame`]s in, NV12 [`Frame`]s out.
//!
//! - [`mf`]: Media Foundation decoder MFTs (H.264, H.265, MJPEG), synchronous software mode.
//! - [`openh264`](mod@self::openh264): Cisco OpenH264, a software fallback for H.264.
//!
//! Decoders are not `Send`: Media Foundation objects are created and used on the pipeline's
//! decode thread only.

#[cfg(windows)]
pub mod mf;
#[cfg(feature = "openh264")]
pub mod openh264;

use std::collections::VecDeque;
use std::fmt;
use std::str::FromStr;
use std::time::{Duration, Instant};

use crate::error::PipelineError;
use crate::{EncodedFrame, Frame, VideoCodec};

/// Turns encoded frames into pictures.
pub trait Decoder {
    /// Shown in logs and the UI, for example "Media Foundation H.264".
    fn name(&self) -> &str;

    /// Decodes one access unit and appends any finished pictures to `out` (decoders with
    /// reordering may return none for a while, or several at once).
    fn decode(&mut self, frame: &EncodedFrame, out: &mut Vec<Frame>) -> Result<(), PipelineError>;
}

/// Which decoder implementation to use.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum DecoderChoice {
    /// Media Foundation, falling back to OpenH264 for H.264 if MF isn't available.
    #[default]
    Auto,
    MediaFoundation,
    OpenH264,
}

impl FromStr for DecoderChoice {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "auto" => Ok(Self::Auto),
            "mf" | "mediafoundation" | "media-foundation" => Ok(Self::MediaFoundation),
            "openh264" => Ok(Self::OpenH264),
            _ => Err(format!(
                "unknown decoder \"{s}\" (expected auto, mf or openh264)"
            )),
        }
    }
}

impl fmt::Display for DecoderChoice {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Auto => "auto",
            Self::MediaFoundation => "mf",
            Self::OpenH264 => "openh264",
        })
    }
}

/// Creates a decoder for `codec`. `size` is the stream's picture size if already known; some
/// decoders (MJPEG) need it up front.
pub fn create_decoder(
    codec: VideoCodec,
    size: Option<(u32, u32)>,
    choice: DecoderChoice,
) -> Result<Box<dyn Decoder>, PipelineError> {
    match choice {
        DecoderChoice::MediaFoundation => create_mf(codec, size),
        DecoderChoice::OpenH264 => create_openh264(codec),
        DecoderChoice::Auto => match create_mf(codec, size) {
            Ok(d) => Ok(d),
            Err(e) if codec == VideoCodec::H264 && cfg!(feature = "openh264") => {
                tracing::warn!(error = %e, "Media Foundation H.264 decoder unavailable; using OpenH264");
                create_openh264(codec)
            }
            Err(e) => Err(e),
        },
    }
}

#[cfg(windows)]
fn create_mf(
    codec: VideoCodec,
    size: Option<(u32, u32)>,
) -> Result<Box<dyn Decoder>, PipelineError> {
    Ok(Box::new(mf::MfDecoder::new(codec, size)?))
}

#[cfg(not(windows))]
fn create_mf(_: VideoCodec, _: Option<(u32, u32)>) -> Result<Box<dyn Decoder>, PipelineError> {
    Err(PipelineError::new(
        crate::error::ErrorKind::DecoderUnavailable,
        "Media Foundation is only available on Windows",
    ))
}

#[cfg(feature = "openh264")]
fn create_openh264(codec: VideoCodec) -> Result<Box<dyn Decoder>, PipelineError> {
    Ok(Box::new(self::openh264::OpenH264Decoder::new(codec)?))
}

#[cfg(not(feature = "openh264"))]
fn create_openh264(_: VideoCodec) -> Result<Box<dyn Decoder>, PipelineError> {
    Err(PipelineError::new(
        crate::error::ErrorKind::DecoderUnavailable,
        "this build doesn't include the OpenH264 decoder",
    ))
}

/// Remembers when recent inputs arrived, so outputs (which may come later or reordered) can
/// be stamped with the receive time of the input that produced them.
#[derive(Debug, Default)]
pub(crate) struct ArrivalLog {
    entries: VecDeque<(Duration, Instant)>,
}

impl ArrivalLog {
    const CAPACITY: usize = 64;

    pub(crate) fn push(&mut self, pts: Duration, received: Instant) {
        if self.entries.len() == Self::CAPACITY {
            self.entries.pop_front();
        }
        self.entries.push_back((pts, received));
    }

    /// The receive time of the input with this pts, or of the latest input.
    pub(crate) fn received(&self, pts: Duration) -> Instant {
        self.entries
            .iter()
            .rev()
            .find(|(p, _)| *p == pts)
            .or(self.entries.back())
            .map_or_else(Instant::now, |(_, r)| *r)
    }
}

/// Stamps a decoded picture with its timing.
pub(crate) fn stamp(mut frame: Frame, pts: Duration, arrivals: &ArrivalLog) -> Frame {
    frame.pts = pts;
    frame.received = arrivals.received(pts);
    frame.decoded = Instant::now();
    frame
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_choice() {
        assert_eq!("MF".parse(), Ok(DecoderChoice::MediaFoundation));
        assert_eq!("openh264".parse(), Ok(DecoderChoice::OpenH264));
        assert!("ffmpeg".parse::<DecoderChoice>().is_err());
    }

    #[test]
    fn arrival_log_matches_pts() {
        let mut log = ArrivalLog::default();
        let t0 = Instant::now();
        let t1 = t0 + Duration::from_millis(5);
        log.push(Duration::from_millis(0), t0);
        log.push(Duration::from_millis(33), t1);
        assert_eq!(log.received(Duration::from_millis(0)), t0);
        assert_eq!(log.received(Duration::from_millis(99)), t1);
    }
}
