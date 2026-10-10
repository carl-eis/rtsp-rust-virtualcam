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
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use crate::error::{ErrorKind, PipelineError};
use crate::{EncodedFrame, Frame, VideoCodec};

/// Turns encoded frames into pictures.
pub trait Decoder {
    /// Shown in logs and the UI, for example "Media Foundation H.264".
    fn name(&self) -> &str;

    /// Decodes one access unit and appends any finished pictures to `out` (decoders with
    /// reordering may return none for a while, or several at once).
    ///
    /// [`ErrorKind::DecoderLost`] means this decoder can't go on; the pipeline drops it and
    /// creates a new one at the next key frame. Other errors skip to the next key frame with
    /// the same decoder.
    fn decode(&mut self, frame: &EncodedFrame, out: &mut Vec<Frame>) -> Result<(), PipelineError>;

    /// Asks for pictures no larger than needed to cover `max` (width, height): a decoder that
    /// can shrink pictures cheaply (on the GPU) scales them, keeping their shape, by
    /// `min(1, max(max_w / width, max_h / height))`, so the result is at least `max_w` wide or
    /// at least `max_h` high. `None` means full size. Decoders that can't do it cheaply ignore
    /// it (the default); the camera's scaler still makes the exact output size.
    fn limit_size(&mut self, _max: Option<(u32, u32)>) {}
}

/// The size a picture of `width` x `height` shrinks to under [`Decoder::limit_size`]: the
/// smallest even size with the same shape that still covers `max` in one direction, never
/// larger than the picture.
pub fn limited_size(width: u32, height: u32, max: Option<(u32, u32)>) -> (u32, u32) {
    let Some((mw, mh)) = max else {
        return (width, height);
    };
    if width == 0 || height == 0 || mw >= width || mh >= height {
        return (width, height);
    }
    let (w, h, mw, mh) = (
        u64::from(width),
        u64::from(height),
        u64::from(mw),
        u64::from(mh),
    );
    // The larger of mw / w and mh / h decides, compared without division.
    let (sw, sh) = if mw * h >= mh * w {
        (mw, (h * mw).div_ceil(w))
    } else {
        ((w * mh).div_ceil(h), mh)
    };
    let even_up = |v: u64| (v.max(2) + 1) & !1;
    (even_up(sw).min(w) as u32, even_up(sh).min(h) as u32)
}

/// What hardware decoding this computer has, for the "Use hardware decoding" setting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HardwareDecoding {
    /// Still finding out (the check runs on a background thread at startup).
    Checking,
    /// A device that can decode; `codecs` are the ones it takes.
    Available {
        device: String,
        codecs: Vec<VideoCodec>,
    },
    /// Why not, in words for the user.
    Unavailable(String),
}

impl HardwareDecoding {
    pub fn is_available(&self) -> bool {
        matches!(self, Self::Available { .. })
    }

    /// One line for the UI and logs, for example "NVIDIA GeForce RTX 3060 (H.264, H.265)".
    pub fn describe(&self) -> String {
        match self {
            Self::Checking => "Checking…".to_owned(),
            Self::Available { device, codecs } => {
                let codecs: Vec<&str> = codecs.iter().map(|c| c.name()).collect();
                format!("{device} ({})", codecs.join(", "))
            }
            Self::Unavailable(why) => why.clone(),
        }
    }
}

/// The OS's own video decoders (system codecs, possibly hardware).
///
/// - Windows: Media Foundation decoder MFTs (H.264, H.265 with the HEVC extension, MJPEG),
///   decoding on the GPU through Direct3D 11 when hardware decoding is on.
/// - Linux and macOS: none yet; H.264 goes to OpenH264. VA-API or VideoToolbox would go here.
pub trait PlatformDecoders: Send + Sync + 'static {
    /// Creates a decoder for `codec`. `size` is the stream's picture size if already known;
    /// some decoders (MJPEG) need it up front. `hardware: false` asks for a software decoder
    /// even when hardware decoding is on (this stream's hardware decoder failed). Called on
    /// the decode thread.
    fn create(
        &self,
        codec: VideoCodec,
        size: Option<(u32, u32)>,
        hardware: bool,
    ) -> Result<Box<dyn Decoder>, PipelineError>;

    /// What to tell the user when no decoder handles a codec, if this OS has advice.
    fn unavailable_hint(&self) -> Option<&'static str> {
        None
    }

    /// What hardware decoding this computer has. Found out once, then remembered.
    fn hardware(&self) -> HardwareDecoding {
        HardwareDecoding::Unavailable("Not available on this system yet".to_owned())
    }

    /// Turns hardware decoding on or off for decoders created from now on.
    fn set_use_hardware(&self, _on: bool) {}
}

static PLATFORM: OnceLock<Box<dyn PlatformDecoders>> = OnceLock::new();

/// Whether hardware decoding was asked for (off until someone turns it on).
static USE_HARDWARE: AtomicBool = AtomicBool::new(false);
/// Bumped when [`set_use_hardware`] changes the setting, so running pipelines replace their
/// decoders.
static GENERATION: AtomicU64 = AtomicU64::new(0);

/// What hardware decoding this computer has (`Unavailable` when no platform decoders are
/// installed).
pub fn hardware_decoding() -> HardwareDecoding {
    platform().map_or_else(
        || HardwareDecoding::Unavailable("Not available on this system yet".to_owned()),
        PlatformDecoders::hardware,
    )
}

/// Turns hardware decoding on or off. Off by default in every process. Running pipelines
/// switch decoders at their next key frame, without reconnecting.
pub fn set_use_hardware(on: bool) {
    if let Some(p) = platform() {
        p.set_use_hardware(on);
    }
    if USE_HARDWARE.swap(on, Ordering::SeqCst) != on {
        GENERATION.fetch_add(1, Ordering::SeqCst);
        tracing::info!(on, "hardware decoding switched");
    }
}

/// Whether hardware decoding is turned on (not whether it is available).
pub fn uses_hardware() -> bool {
    USE_HARDWARE.load(Ordering::SeqCst)
}

/// Changes whenever decoders created before should be replaced.
pub fn generation() -> u64 {
    GENERATION.load(Ordering::SeqCst)
}

/// Held by tests that flip the process-wide hardware switch.
#[cfg(test)]
pub(crate) static HARDWARE_SWITCH: std::sync::Mutex<()> = std::sync::Mutex::new(());

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
    create_decoder_with(codec, size, choice, true)
}

/// [`create_decoder`], with `hardware: false` to insist on software decoding.
pub fn create_decoder_with(
    codec: VideoCodec,
    size: Option<(u32, u32)>,
    choice: DecoderChoice,
    hardware: bool,
) -> Result<Box<dyn Decoder>, PipelineError> {
    match choice {
        DecoderChoice::Platform => create_platform(codec, size, hardware),
        DecoderChoice::OpenH264 => create_openh264(codec),
        DecoderChoice::Auto => match create_platform(codec, size, hardware) {
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
    hardware: bool,
) -> Result<Box<dyn Decoder>, PipelineError> {
    match platform() {
        Some(p) => p.create(codec, size, hardware),
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
    fn limited_sizes_cover_the_limit_and_keep_the_shape() {
        assert_eq!(limited_size(3840, 2160, None), (3840, 2160));
        assert_eq!(limited_size(3840, 2160, Some((1920, 1080))), (1920, 1080));
        // Never larger than the picture.
        assert_eq!(limited_size(1280, 720, Some((1920, 1080))), (1280, 720));
        // A 4:3 picture for a 16:9 limit covers the width.
        assert_eq!(limited_size(2560, 1920, Some((1280, 720))), (1280, 960));
        // A portrait limit (a rotated camera) covers the height.
        assert_eq!(limited_size(3840, 2160, Some((720, 1280))), (2276, 1280));
        // Odd results round up to even.
        let (w, h) = limited_size(2592, 1944, Some((641, 0)));
        assert!(w >= 641 && w % 2 == 0 && h % 2 == 0, "{w}x{h}");
    }

    #[test]
    fn hardware_switch_bumps_the_generation_on_change_only() {
        let _lock = HARDWARE_SWITCH
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        set_use_hardware(false);
        let g = generation();
        set_use_hardware(true);
        assert!(uses_hardware());
        assert_eq!(generation(), g + 1);
        set_use_hardware(true);
        assert_eq!(generation(), g + 1);
        set_use_hardware(false);
        assert_eq!(generation(), g + 2);
        // Without platform decoders there is no hardware decoding.
        assert!(!hardware_decoding().is_available());
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
