//! Video decoders: compressed [`EncodedFrame`]s in, NV12 [`Frame`]s out.
//!
//! - [`openh264`](mod@self::openh264): Cisco OpenH264, software H.264 on every OS.
//! - [`PlatformDecoders`]: the OS's own decoders, installed at startup with
//!   [`install_platform_decoders`] (`rtspcam-platform` installs Media Foundation on Windows).
//!
//! Decoders are not `Send`: platform decoder objects are created and used on the pipeline's
//! decode thread only.

#[cfg(feature = "openh264")]
pub mod openh264;

use std::collections::VecDeque;
use std::fmt;
use std::str::FromStr;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use crate::error::{ErrorKind, PipelineError};
use crate::{EncodedFrame, Frame, VideoCodec};

/// Turns encoded frames into pictures.
pub trait Decoder {
    /// Shown in logs and the UI, for example "Media Foundation H.264".
    fn name(&self) -> &str;

    /// Decodes one access unit and appends any finished pictures to `out` (decoders with
    /// reordering may return none for a while, or several at once).
    fn decode(&mut self, frame: &EncodedFrame, out: &mut Vec<Frame>) -> Result<(), PipelineError>;
}

/// The OS's own video decoders (system codecs, possibly hardware).
///
/// - Windows: Media Foundation decoder MFTs (H.264, H.265 with the HEVC extension, MJPEG).
/// - Linux and macOS: none yet; H.264 goes to OpenH264. VA-API or VideoToolbox would go here.
pub trait PlatformDecoders: Send + Sync + 'static {
    /// Creates a decoder for `codec`. `size` is the stream's picture size if already known;
    /// some decoders (MJPEG) need it up front. Called on the decode thread.
    fn create(
        &self,
        codec: VideoCodec,
        size: Option<(u32, u32)>,
    ) -> Result<Box<dyn Decoder>, PipelineError>;

    /// What to tell the user when no decoder handles a codec, if this OS has advice.
    fn unavailable_hint(&self) -> Option<&'static str> {
        None
    }
}

static PLATFORM: OnceLock<Box<dyn PlatformDecoders>> = OnceLock::new();

/// Sets the OS decoders used by [`DecoderChoice::Platform`] and [`DecoderChoice::Auto`] in this
/// process. Call once at startup. Returns `false` (keeping the first) if some were installed.
pub fn install_platform_decoders(decoders: Box<dyn PlatformDecoders>) -> bool {
    PLATFORM.set(decoders).is_ok()
}

fn platform() -> Option<&'static dyn PlatformDecoders> {
    PLATFORM.get().map(Box::as_ref)
}

/// Advice for [`ErrorKind::DecoderUnavailable`].
pub(crate) fn unavailable_hint() -> &'static str {
    platform()
        .and_then(PlatformDecoders::unavailable_hint)
        .unwrap_or("Switch the camera (or its sub stream) to H.264.")
}

/// Which decoder implementation to use.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum DecoderChoice {
    /// The platform's decoders, falling back to OpenH264 for H.264 if they can't decode it.
    #[default]
    Auto,
    /// The platform's decoders only (Media Foundation on Windows).
    Platform,
    OpenH264,
}

impl FromStr for DecoderChoice {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "auto" => Ok(Self::Auto),
            "platform" | "mf" | "mediafoundation" | "media-foundation" => Ok(Self::Platform),
            "openh264" => Ok(Self::OpenH264),
            _ => Err(format!(
                "unknown decoder \"{s}\" (expected auto, platform (or mf) or openh264)"
            )),
        }
    }
}

impl fmt::Display for DecoderChoice {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Auto => "auto",
            Self::Platform => "platform",
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
        DecoderChoice::Platform => create_platform(codec, size),
        DecoderChoice::OpenH264 => create_openh264(codec),
        DecoderChoice::Auto => match create_platform(codec, size) {
            Ok(d) => Ok(d),
            Err(e) if codec == VideoCodec::H264 && cfg!(feature = "openh264") => {
                if platform().is_some() {
                    tracing::warn!(error = %e, "platform H.264 decoder unavailable; using OpenH264");
                }
                create_openh264(codec)
            }
            Err(e) => Err(e),
        },
    }
}

fn create_platform(
    codec: VideoCodec,
    size: Option<(u32, u32)>,
) -> Result<Box<dyn Decoder>, PipelineError> {
    match platform() {
        Some(p) => p.create(codec, size),
        None => Err(PipelineError::new(
            ErrorKind::DecoderUnavailable,
            format!("there is no {} decoder on this platform", codec.name()),
        )),
    }
}

#[cfg(feature = "openh264")]
fn create_openh264(codec: VideoCodec) -> Result<Box<dyn Decoder>, PipelineError> {
    Ok(Box::new(self::openh264::OpenH264Decoder::new(codec)?))
}

#[cfg(not(feature = "openh264"))]
fn create_openh264(_: VideoCodec) -> Result<Box<dyn Decoder>, PipelineError> {
    Err(PipelineError::new(
        ErrorKind::DecoderUnavailable,
        "this build doesn't include the OpenH264 decoder",
    ))
}

/// Remembers when recent inputs arrived, so outputs (which may come later or reordered) can
/// be stamped with the receive time of the input that produced them.
#[derive(Debug, Default)]
pub struct ArrivalLog {
    entries: VecDeque<(Duration, Instant)>,
}

impl ArrivalLog {
    const CAPACITY: usize = 64;

    pub fn push(&mut self, pts: Duration, received: Instant) {
        if self.entries.len() == Self::CAPACITY {
            self.entries.pop_front();
        }
        self.entries.push_back((pts, received));
    }

    /// The receive time of the input with this pts, or of the latest input.
    pub fn received(&self, pts: Duration) -> Instant {
        self.entries
            .iter()
            .rev()
            .find(|(p, _)| *p == pts)
            .or(self.entries.back())
            .map_or_else(Instant::now, |(_, r)| *r)
    }
}

/// Stamps a decoded picture with its timing.
pub fn stamp(mut frame: Frame, pts: Duration, arrivals: &ArrivalLog) -> Frame {
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
        assert_eq!("MF".parse(), Ok(DecoderChoice::Platform));
        assert_eq!("platform".parse(), Ok(DecoderChoice::Platform));
        assert_eq!("openh264".parse(), Ok(DecoderChoice::OpenH264));
        assert!("ffmpeg".parse::<DecoderChoice>().is_err());
    }

    #[test]
    fn without_platform_decoders_other_codecs_are_unavailable() {
        // This test binary installs none.
        let err = create_decoder(VideoCodec::H265, None, DecoderChoice::Auto)
            .err()
            .unwrap();
        assert_eq!(err.kind(), ErrorKind::DecoderUnavailable);
        assert!(err.to_string().contains("H.265"), "{err}");
        assert!(err.kind().hint().unwrap().contains("H.264"));
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
