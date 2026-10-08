//! RTSP ingest with `retina`: DESCRIBE / SETUP / PLAY, then a stream of encoded video frames.

use std::fmt;
use std::pin::Pin;
use std::time::{Duration, Instant};

use futures::StreamExt as _;
use retina::client::{Demuxed, PlayOptions, SessionOptions, SetupOptions};
use retina::codec::{CodecItem, FrameFormat, ParametersRef, VideoParametersCodec};
use rtspcam_core::config::parse_stream_url;
use rtspcam_core::{StreamConfig, Transport};
use url::Url;
use zeroize::Zeroizing;

use crate::error::{ErrorKind, PipelineError};

/// Video codecs RTSP Cam can decode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum VideoCodec {
    H264,
    H265,
    Mjpeg,
}

impl VideoCodec {
    /// From an SDP `rtpmap` encoding name, as reported by retina (lowercase).
    fn from_encoding(name: &str) -> Option<Self> {
        match name {
            "h264" => Some(Self::H264),
            "h265" => Some(Self::H265),
            "jpeg" => Some(Self::Mjpeg),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::H264 => "H.264",
            Self::H265 => "H.265",
            Self::Mjpeg => "MJPEG",
        }
    }
}

impl fmt::Display for VideoCodec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// One compressed picture. H.264/H.265 data is Annex B with parameter sets in front of every
/// key frame; MJPEG data is a complete JPEG file.
#[derive(Debug, Clone)]
pub struct EncodedFrame {
    pub codec: VideoCodec,
    pub data: Vec<u8>,
    /// A key frame (IDR / JPEG): decoding can start here.
    pub keyframe: bool,
    /// Time since the start of the session, from the RTP timestamp.
    pub pts: Duration,
    /// When the last packet of the frame arrived.
    pub received: Instant,
    /// RTP packets lost just before this frame.
    pub loss: u16,
}

/// What the server told us about the video stream.
#[derive(Debug, Clone, PartialEq)]
pub struct StreamInfo {
    pub codec: VideoCodec,
    /// RFC 6381 codec string such as `avc1.4D401F`, once parameters are known.
    pub codec_string: Option<String>,
    /// Display size, once parameters are known (always after the first frame).
    pub size: Option<(u32, u32)>,
    /// Frame rate the stream declares, if any.
    pub fps: Option<f32>,
    /// Server software from the SDP, if declared.
    pub tool: Option<String>,
}

/// User name and password. `Debug` never shows the password.
#[derive(Clone)]
pub struct Credentials {
    pub username: String,
    pub password: Zeroizing<String>,
}

impl fmt::Debug for Credentials {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Credentials")
            .field("username", &self.username)
            .field("password", &"<redacted>")
            .finish()
    }
}

/// Everything needed to open a stream.
#[derive(Debug, Clone)]
pub struct SourceOptions {
    /// `rtsp://host:port/path`, without credentials.
    pub url: Url,
    pub credentials: Option<Credentials>,
    pub transport: Transport,
    /// Limit for DESCRIBE + SETUP + PLAY together.
    pub connect_timeout: Duration,
}

impl SourceOptions {
    pub const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

    pub fn new(url: Url) -> Self {
        Self {
            url,
            credentials: None,
            transport: Transport::Tcp,
            connect_timeout: Self::DEFAULT_CONNECT_TIMEOUT,
        }
    }

    /// From a configured stream. Fails if the protocol is unsupported, the address doesn't
    /// form a valid URL, or the saved password can't be decrypted.
    pub fn from_stream(stream: &StreamConfig) -> Result<Self, PipelineError> {
        if !stream.protocol.is_supported() {
            return Err(PipelineError::new(
                ErrorKind::InvalidConfig,
                format!("unsupported protocol \"{}\"", stream.protocol),
            ));
        }
        let url = Url::parse(&stream.url()).map_err(|e| {
            PipelineError::new(ErrorKind::InvalidConfig, format!("invalid address: {e}"))
        })?;
        let credentials = if stream.username.trim().is_empty() {
            None
        } else {
            let (user, pass) = stream.credentials().ok_or_else(|| {
                PipelineError::new(
                    ErrorKind::InvalidConfig,
                    "the saved password can't be decrypted; enter it again",
                )
            })?;
            Some(Credentials {
                username: user.to_owned(),
                password: Zeroizing::new(pass.to_owned()),
            })
        };
        Ok(Self {
            credentials,
            transport: stream.transport,
            ..Self::new(url)
        })
    }

    /// From a URL that may contain credentials (`rtsp://user:pass@host/path`).
    pub fn from_url(input: &str) -> Result<Self, PipelineError> {
        let parts = parse_stream_url(input)
            .map_err(|e| PipelineError::new(ErrorKind::InvalidConfig, e.to_string()))?;
        let mut stream = StreamConfig {
            port: parts.port,
            path: parts.path,
            username: parts.username,
            ..StreamConfig::new("url", parts.host)
        };
        stream.password = parts.password.map(rtspcam_core::Secret::new);
        Self::from_stream(&stream)
    }
}

/// A playing RTSP session, yielding encoded frames of its video stream.
pub struct RtspSource {
    demuxed: Pin<Box<Demuxed>>,
    stream_index: usize,
    info: StreamInfo,
    /// H.265 only: VPS + SPS + PPS in Annex B, for key frames retina doesn't recognize.
    h265_params: Option<Vec<u8>>,
}

impl fmt::Debug for RtspSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RtspSource")
            .field("stream_index", &self.stream_index)
            .field("info", &self.info)
            .finish_non_exhaustive()
    }
}

impl RtspSource {
    /// Connects and starts playing the first supported video stream.
    pub async fn connect(opts: &SourceOptions) -> Result<Self, PipelineError> {
        tokio::time::timeout(opts.connect_timeout, Self::connect_inner(opts))
            .await
            .map_err(|_| {
                PipelineError::new(
                    ErrorKind::Timeout,
                    format!(
                        "no answer from {} within {} s",
                        host_port(&opts.url),
                        opts.connect_timeout.as_secs()
                    ),
                )
            })?
    }

    async fn connect_inner(opts: &SourceOptions) -> Result<Self, PipelineError> {
        let session_opts = SessionOptions::default()
            .creds(
                opts.credentials
                    .as_ref()
                    .map(|c| retina::client::Credentials {
                        username: c.username.clone(),
                        password: c.password.to_string(),
                    }),
            )
            .user_agent(format!("RTSP Cam/{}", env!("CARGO_PKG_VERSION")));
        let mut session = retina::client::Session::describe(opts.url.clone(), session_opts).await?;

        let (stream_index, codec) = pick_video_stream(session.streams())?;
        let transport = match opts.transport {
            Transport::Tcp => retina::client::Transport::Tcp(Default::default()),
            Transport::Udp => retina::client::Transport::Udp(Default::default()),
        };
        session
            .setup(
                stream_index,
                SetupOptions::default()
                    .transport(transport)
                    .frame_format(FrameFormat::SIMPLE),
            )
            .await?;
        let demuxed = session.play(PlayOptions::default()).await?.demuxed()?;

        let mut source = Self {
            demuxed: Box::pin(demuxed),
            stream_index,
            info: StreamInfo {
                codec,
                codec_string: None,
                size: None,
                fps: None,
                tool: None,
            },
            h265_params: None,
        };
        source.info.tool = source.demuxed.tool().map(|t| t.to_string());
        source.refresh_info();
        Ok(source)
    }

    pub fn info(&self) -> &StreamInfo {
        &self.info
    }

    /// The next video frame. `Ok(None)` means the server ended the session.
    pub async fn next_frame(&mut self) -> Result<Option<EncodedFrame>, PipelineError> {
        loop {
            let item = match self.demuxed.next().await {
                None => return Ok(None),
                Some(item) => item?,
            };
            let CodecItem::VideoFrame(frame) = item else {
                continue;
            };
            if frame.stream_id() != self.stream_index {
                continue;
            }
            if frame.has_new_parameters() {
                self.refresh_info();
            }
            let pts = Duration::from_secs_f64(frame.timestamp().elapsed_secs().max(0.0));
            let received = frame.end_ctx().received();
            let loss = frame.loss();
            let mut keyframe = frame.is_random_access_point();
            let mut data = frame.into_data();
            // retina only flags H.265 IDR pictures as random access points (and only prepends
            // parameter sets to those). Open-GOP encoders use CRA pictures instead, so a decoder
            // joining mid-stream would never start. Detect any IRAP picture and add the
            // parameter sets ourselves.
            if self.info.codec == VideoCodec::H265 && !keyframe && h265_has_irap(&data) {
                keyframe = true;
                if let Some(params) = &self.h265_params {
                    data.splice(0..0, params.iter().copied());
                }
            }
            return Ok(Some(EncodedFrame {
                codec: self.info.codec,
                keyframe,
                pts,
                received,
                loss,
                data,
            }));
        }
    }

    fn refresh_info(&mut self) {
        let stream = &self.demuxed.streams()[self.stream_index];
        if let Some(fps) = stream.framerate() {
            self.info.fps = Some(fps);
        }
        if let Some(ParametersRef::Video(params)) = stream.parameters() {
            self.info.size = Some(params.pixel_dimensions());
            self.info.codec_string = Some(params.rfc6381_codec().to_owned());
            if let VideoParametersCodec::H265 { vps, sps, pps } = params.codec_params() {
                let mut annex_b = Vec::with_capacity(12 + vps.len() + sps.len() + pps.len());
                for nal in [vps, sps, pps] {
                    annex_b.extend_from_slice(&[0, 0, 0, 1]);
                    annex_b.extend_from_slice(nal);
                }
                self.h265_params = Some(annex_b);
            }
            if let Some((num, den)) = params.frame_rate()
                && num > 0
            {
                self.info.fps = Some(den as f32 / num as f32);
            }
        }
    }
}

fn pick_video_stream(
    streams: &[retina::client::Stream],
) -> Result<(usize, VideoCodec), PipelineError> {
    let mut unsupported = None;
    for (i, s) in streams.iter().enumerate() {
        if s.media() != "video" {
            continue;
        }
        match VideoCodec::from_encoding(s.encoding_name()) {
            Some(codec) => return Ok((i, codec)),
            None => unsupported = unsupported.or(Some(s.encoding_name().to_owned())),
        }
    }
    Err(match unsupported {
        Some(name) => PipelineError::new(
            ErrorKind::UnsupportedCodec,
            format!("the video codec \"{name}\" is not supported"),
        ),
        None => PipelineError::new(ErrorKind::NoVideo, "the stream has no video track"),
    })
}

/// The NAL units of an Annex B byte stream (without start codes).
pub(crate) fn annex_b_nals(data: &[u8]) -> impl Iterator<Item = &[u8]> {
    let mut rest = data;
    std::iter::from_fn(move || {
        let start = rest.windows(3).position(|w| w == [0, 0, 1])? + 3;
        let body = &rest[start..];
        let end = body
            .windows(3)
            .position(|w| w == [0, 0, 1])
            .unwrap_or(body.len());
        rest = &body[end..];
        // A 4-byte start code leaves its leading zero at the end of the previous NAL.
        let mut nal = &body[..end];
        while let [head @ .., 0] = nal {
            nal = head;
        }
        Some(nal)
    })
}

/// Whether an H.265 access unit contains an IRAP picture (BLA, IDR or CRA: NAL types 16-23).
fn h265_has_irap(data: &[u8]) -> bool {
    annex_b_nals(data).any(|nal| {
        nal.first()
            .is_some_and(|b| (16..=23).contains(&((b >> 1) & 0x3f)))
    })
}

fn host_port(url: &Url) -> String {
    match (url.host_str(), url.port()) {
        (Some(h), Some(p)) => format!("{h}:{p}"),
        (Some(h), None) => h.to_owned(),
        _ => url.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use rtspcam_core::Secret;

    use super::*;

    #[test]
    fn options_from_stream_keep_credentials_out_of_the_url() {
        let mut s = StreamConfig::new("cam", "192.168.1.50");
        s.path = "/live".into();
        s.username = "admin".into();
        s.password = Some(Secret::new("hunter2"));
        s.transport = Transport::Udp;
        let o = SourceOptions::from_stream(&s).unwrap();
        assert_eq!(o.url.as_str(), "rtsp://192.168.1.50:554/live");
        let c = o.credentials.as_ref().unwrap();
        assert_eq!(
            (c.username.as_str(), c.password.as_str()),
            ("admin", "hunter2")
        );
        assert_eq!(o.transport, Transport::Udp);
        assert!(!format!("{o:?}").contains("hunter2"));
    }

    #[test]
    fn options_from_url() {
        let o =
            SourceOptions::from_url("rtsp://rtspcam:rtspcam-test@127.0.0.1:8554/secure").unwrap();
        assert_eq!(o.url.as_str(), "rtsp://127.0.0.1:8554/secure");
        assert_eq!(o.credentials.unwrap().password.as_str(), "rtspcam-test");

        let o = SourceOptions::from_url("rtsp://127.0.0.1:8554/h264-720p").unwrap();
        assert!(o.credentials.is_none());

        let e = SourceOptions::from_url("http://x/y").unwrap_err();
        assert_eq!(e.kind(), ErrorKind::InvalidConfig);
    }

    #[test]
    fn user_without_password_is_allowed() {
        let mut s = StreamConfig::new("cam", "cam.local");
        s.username = "viewer".into();
        let o = SourceOptions::from_stream(&s).unwrap();
        assert_eq!(o.credentials.unwrap().password.as_str(), "");
    }

    #[test]
    fn splits_annex_b() {
        let data = [
            0, 0, 0, 1, 0x40, 1, 0, 0, 1, 0x42, 2, 0, 0, 0, 1, 0x2a, 3, 0,
        ];
        let nals: Vec<&[u8]> = annex_b_nals(&data).collect();
        assert_eq!(nals, [&[0x40, 1][..], &[0x42, 2], &[0x2a, 3]]);
        assert_eq!(annex_b_nals(&[1, 2, 3]).count(), 0);
    }

    #[test]
    fn detects_h265_irap() {
        // NAL header byte = type << 1. CRA = 21, IDR_W_RADL = 19, TRAIL_R = 1, VPS = 32.
        let cra = [0, 0, 0, 1, 21 << 1, 1, 0xaa];
        let idr = [0, 0, 1, 19 << 1, 1, 0xaa];
        let trail = [0, 0, 0, 1, 32 << 1, 1, 0, 0, 1, 1 << 1, 1, 0xaa];
        assert!(h265_has_irap(&cra));
        assert!(h265_has_irap(&idr));
        assert!(!h265_has_irap(&trail));
    }

    #[test]
    fn codec_names() {
        assert_eq!(VideoCodec::from_encoding("h264"), Some(VideoCodec::H264));
        assert_eq!(VideoCodec::from_encoding("jpeg"), Some(VideoCodec::Mjpeg));
        assert_eq!(VideoCodec::from_encoding("mp4v-es"), None);
    }
}
