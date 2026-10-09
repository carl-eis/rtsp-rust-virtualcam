//! Wire format between the app (pipe server) and the virtual camera DLL (pipe client).
//!
//! Every message is a 16-byte header followed by a body:
//!
//! ```text
//! offset  size  field
//!      0     4  magic "RCAM"
//!      4     2  protocol version (1)
//!      6     2  kind (see `Kind`)
//!      8     4  body length in bytes
//!     12     4  reserved (0)
//! ```
//!
//! All integers are little-endian. The DLL runs inside the Frame Server service, so everything
//! it reads is validated: sizes are bounded before anything is allocated, and a frame must
//! match the format the DLL asked for exactly.

use std::fmt;
use std::io::{self, Read, Write};

pub const MAGIC: [u8; 4] = *b"RCAM";
pub const VERSION: u16 = 1;
pub const HEADER_LEN: usize = 16;

pub const MAX_WIDTH: u32 = 4096;
pub const MAX_HEIGHT: u32 = 2160;
pub const MAX_FPS: u32 = 120;
const FRAME_HEADER_LEN: usize = 32;
const MAX_STATUS_MESSAGE: usize = 1024;
/// Largest body any message may have: a 4096x2160 RGB32 frame plus its header.
pub const MAX_BODY_LEN: usize = FRAME_HEADER_LEN + (MAX_WIDTH * MAX_HEIGHT * 4) as usize;

/// Pixel layout of frame data.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PixelFormat {
    /// Y plane then interleaved UV, rows `width` bytes.
    Nv12,
    /// B, G, R, X bytes per pixel, top-down, rows `width * 4` bytes.
    Rgb32,
}

impl PixelFormat {
    fn code(self) -> u32 {
        match self {
            Self::Nv12 => 0,
            Self::Rgb32 => 1,
        }
    }

    fn from_code(code: u32) -> Option<Self> {
        match code {
            0 => Some(Self::Nv12),
            1 => Some(Self::Rgb32),
            _ => None,
        }
    }
}

/// The video format a camera consumer selected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct VideoFormat {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub pixel_format: PixelFormat,
}

impl VideoFormat {
    /// Bytes in one frame of this format.
    pub fn frame_len(&self) -> usize {
        let (w, h) = (self.width as usize, self.height as usize);
        match self.pixel_format {
            PixelFormat::Nv12 => w * h * 3 / 2,
            PixelFormat::Rgb32 => w * h * 4,
        }
    }

    fn validate(&self) -> Result<(), ProtocolError> {
        let ok = (2..=MAX_WIDTH).contains(&self.width)
            && (2..=MAX_HEIGHT).contains(&self.height)
            && self.width.is_multiple_of(2)
            && self.height.is_multiple_of(2)
            && (1..=MAX_FPS).contains(&self.fps);
        if ok {
            Ok(())
        } else {
            Err(ProtocolError::Invalid(format!("bad video format {self}")))
        }
    }

    fn encode(&self, out: &mut Vec<u8>) {
        for v in [self.width, self.height, self.fps, self.pixel_format.code()] {
            out.extend_from_slice(&v.to_le_bytes());
        }
    }

    fn decode(b: &[u8]) -> Result<Self, ProtocolError> {
        let f = Self {
            width: u32_at(b, 0)?,
            height: u32_at(b, 4)?,
            fps: u32_at(b, 8)?,
            pixel_format: PixelFormat::from_code(u32_at(b, 12)?)
                .ok_or_else(|| ProtocolError::Invalid("unknown pixel format".into()))?,
        };
        f.validate()?;
        Ok(f)
    }
}

impl fmt::Display for VideoFormat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{:?} {}x{} @ {} fps",
            self.pixel_format, self.width, self.height, self.fps
        )
    }
}

/// What the app's stream is doing, so the camera can show a matching placeholder.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StreamStatus {
    /// Frames are flowing.
    Streaming,
    /// Connecting to the RTSP source (on demand start, or reconnecting).
    Connecting,
    /// The source failed; the message says why.
    Error,
    /// The stream is disabled in the app.
    Disabled,
}

impl StreamStatus {
    fn code(self) -> u32 {
        match self {
            Self::Streaming => 0,
            Self::Connecting => 1,
            Self::Error => 2,
            Self::Disabled => 3,
        }
    }

    fn from_code(code: u32) -> Self {
        match code {
            0 => Self::Streaming,
            1 => Self::Connecting,
            3 => Self::Disabled,
            _ => Self::Error,
        }
    }
}

/// Metadata in front of every frame's pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameHeader {
    /// Increases by at least one per new picture.
    pub seq: u64,
    /// Capture time in 100 ns units (informational; the DLL stamps its own sample times).
    pub timestamp: i64,
    pub format: VideoFormat,
}

/// A decoded message. Frame pixels borrow the caller's buffer.
#[derive(Debug, PartialEq, Eq)]
pub enum Message<'a> {
    /// Client → server, first message: who is asking and in which format.
    Hello { pid: u32, format: VideoFormat },
    /// Client → server: the consumer switched formats.
    SetFormat(VideoFormat),
    /// Server → client.
    Frame { header: FrameHeader, data: &'a [u8] },
    /// Server → client: stream state, sent on change and as a heartbeat.
    Status {
        status: StreamStatus,
        message: String,
    },
    /// Either side: closing.
    Goodbye,
    /// A kind this version doesn't know; skipped for forward compatibility.
    Unknown(u16),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u16)]
enum Kind {
    Hello = 1,
    SetFormat = 2,
    Frame = 3,
    Status = 4,
    Goodbye = 5,
}

#[derive(Debug, thiserror::Error)]
pub enum ProtocolError {
    #[error("pipe I/O: {0}")]
    Io(#[from] io::Error),
    #[error("bad magic: not an RTSP Cam pipe")]
    BadMagic,
    #[error("peer uses protocol version {0}, this build speaks {VERSION}")]
    Version(u16),
    #[error("message of {0} bytes exceeds the limit")]
    TooLarge(usize),
    #[error("invalid message: {0}")]
    Invalid(String),
}

/// A parsed message header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Header {
    pub kind: u16,
    pub body_len: usize,
}

/// Parses and validates a header.
pub fn parse_header(b: &[u8; HEADER_LEN]) -> Result<Header, ProtocolError> {
    if b[0..4] != MAGIC {
        return Err(ProtocolError::BadMagic);
    }
    let version = u16::from_le_bytes([b[4], b[5]]);
    if version != VERSION {
        return Err(ProtocolError::Version(version));
    }
    let kind = u16::from_le_bytes([b[6], b[7]]);
    let body_len = u32::from_le_bytes([b[8], b[9], b[10], b[11]]) as usize;
    if body_len > MAX_BODY_LEN {
        return Err(ProtocolError::TooLarge(body_len));
    }
    Ok(Header { kind, body_len })
}

/// Parses a body of the given kind.
pub fn parse_body(kind: u16, body: &[u8]) -> Result<Message<'_>, ProtocolError> {
    Ok(match kind {
        k if k == Kind::Hello as u16 => {
            exact_len(body, 4 + 16)?;
            Message::Hello {
                pid: u32_at(body, 0)?,
                format: VideoFormat::decode(&body[4..])?,
            }
        }
        k if k == Kind::SetFormat as u16 => {
            exact_len(body, 16)?;
            Message::SetFormat(VideoFormat::decode(body)?)
        }
        k if k == Kind::Frame as u16 => {
            if body.len() < FRAME_HEADER_LEN {
                return Err(ProtocolError::Invalid("short frame header".into()));
            }
            let format = VideoFormat::decode(&body[16..32])?;
            let data = &body[FRAME_HEADER_LEN..];
            if data.len() != format.frame_len() {
                return Err(ProtocolError::Invalid(format!(
                    "frame has {} bytes, {format} needs {}",
                    data.len(),
                    format.frame_len()
                )));
            }
            Message::Frame {
                header: FrameHeader {
                    seq: u64_at(body, 0)?,
                    timestamp: u64_at(body, 8)? as i64,
                    format,
                },
                data,
            }
        }
        k if k == Kind::Status as u16 => {
            if body.len() < 4 || body.len() > 4 + MAX_STATUS_MESSAGE {
                return Err(ProtocolError::Invalid("bad status length".into()));
            }
            Message::Status {
                status: StreamStatus::from_code(u32_at(body, 0)?),
                message: String::from_utf8_lossy(&body[4..]).into_owned(),
            }
        }
        k if k == Kind::Goodbye as u16 => Message::Goodbye,
        other => Message::Unknown(other),
    })
}

fn exact_len(body: &[u8], len: usize) -> Result<(), ProtocolError> {
    if body.len() == len {
        Ok(())
    } else {
        Err(ProtocolError::Invalid(format!(
            "expected {len} bytes, got {}",
            body.len()
        )))
    }
}

fn u32_at(b: &[u8], at: usize) -> Result<u32, ProtocolError> {
    b.get(at..at + 4)
        .map(|s| u32::from_le_bytes(s.try_into().expect("4 bytes")))
        .ok_or_else(|| ProtocolError::Invalid("truncated".into()))
}

fn u64_at(b: &[u8], at: usize) -> Result<u64, ProtocolError> {
    b.get(at..at + 8)
        .map(|s| u64::from_le_bytes(s.try_into().expect("8 bytes")))
        .ok_or_else(|| ProtocolError::Invalid("truncated".into()))
}

fn header(kind: Kind, body_len: usize) -> [u8; HEADER_LEN] {
    let mut h = [0u8; HEADER_LEN];
    h[0..4].copy_from_slice(&MAGIC);
    h[4..6].copy_from_slice(&VERSION.to_le_bytes());
    h[6..8].copy_from_slice(&(kind as u16).to_le_bytes());
    h[8..12].copy_from_slice(&(body_len as u32).to_le_bytes());
    h
}

/// Encodes a control message (anything but a frame) into header + body bytes.
pub fn encode(msg: &Message<'_>) -> Vec<u8> {
    let mut body = Vec::new();
    let kind = match msg {
        Message::Hello { pid, format } => {
            body.extend_from_slice(&pid.to_le_bytes());
            format.encode(&mut body);
            Kind::Hello
        }
        Message::SetFormat(format) => {
            format.encode(&mut body);
            Kind::SetFormat
        }
        Message::Status { status, message } => {
            body.extend_from_slice(&status.code().to_le_bytes());
            let m = message.as_bytes();
            body.extend_from_slice(&m[..m.len().min(MAX_STATUS_MESSAGE)]);
            Kind::Status
        }
        Message::Goodbye | Message::Unknown(_) => Kind::Goodbye,
        Message::Frame { header, data } => return encode_frame(header, data),
    };
    let mut out = header(kind, body.len()).to_vec();
    out.extend_from_slice(&body);
    out
}

/// The header and frame metadata that go in front of a frame's pixels (`data_len` bytes).
pub fn frame_prefix(h: &FrameHeader, data_len: usize) -> [u8; HEADER_LEN + FRAME_HEADER_LEN] {
    let mut out = [0u8; HEADER_LEN + FRAME_HEADER_LEN];
    out[..HEADER_LEN].copy_from_slice(&header(Kind::Frame, FRAME_HEADER_LEN + data_len));
    out[16..24].copy_from_slice(&h.seq.to_le_bytes());
    out[24..32].copy_from_slice(&h.timestamp.to_le_bytes());
    let mut f = Vec::with_capacity(16);
    h.format.encode(&mut f);
    out[32..48].copy_from_slice(&f);
    out
}

fn encode_frame(h: &FrameHeader, data: &[u8]) -> Vec<u8> {
    let mut out = frame_prefix(h, data.len()).to_vec();
    out.extend_from_slice(data);
    out
}

/// Blocking read of one message into `buf` (reused between calls).
pub fn read_message<'a, R: Read>(
    r: &mut R,
    buf: &'a mut Vec<u8>,
) -> Result<Message<'a>, ProtocolError> {
    let mut h = [0u8; HEADER_LEN];
    r.read_exact(&mut h)?;
    let header = parse_header(&h)?;
    buf.resize(header.body_len, 0);
    r.read_exact(buf)?;
    parse_body(header.kind, buf)
}

/// Blocking write of a control message.
pub fn write_message<W: Write>(w: &mut W, msg: &Message<'_>) -> io::Result<()> {
    w.write_all(&encode(msg))
}

#[cfg(test)]
mod tests {
    use super::*;

    const FMT: VideoFormat = VideoFormat {
        width: 4,
        height: 2,
        fps: 30,
        pixel_format: PixelFormat::Nv12,
    };

    fn round_trip(bytes: &[u8]) -> Message<'static> {
        let mut r = bytes;
        let buf = Box::leak(Box::new(Vec::new()));
        read_message(&mut r, buf).unwrap()
    }

    #[test]
    fn control_messages_round_trip() {
        for msg in [
            Message::Hello {
                pid: 42,
                format: FMT,
            },
            Message::SetFormat(VideoFormat {
                pixel_format: PixelFormat::Rgb32,
                ..FMT
            }),
            Message::Status {
                status: StreamStatus::Error,
                message: "401 Unauthorized".into(),
            },
            Message::Goodbye,
        ] {
            assert_eq!(round_trip(&encode(&msg)), msg);
        }
    }

    #[test]
    fn frame_round_trip() {
        let data: Vec<u8> = (0..12).collect();
        let header = FrameHeader {
            seq: 7,
            timestamp: -5,
            format: FMT,
        };
        let bytes = encode(&Message::Frame {
            header,
            data: &data,
        });
        match round_trip(&bytes) {
            Message::Frame { header: h, data: d } => {
                assert_eq!(h, header);
                assert_eq!(d, &data[..]);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn rejects_wrong_frame_size() {
        let header = FrameHeader {
            seq: 1,
            timestamp: 0,
            format: FMT,
        };
        let bytes = encode(&Message::Frame {
            header,
            data: &[0; 11],
        });
        let mut r = &bytes[..];
        assert!(matches!(
            read_message(&mut r, &mut Vec::new()),
            Err(ProtocolError::Invalid(_))
        ));
    }

    #[test]
    fn rejects_bad_headers() {
        let mut bytes = encode(&Message::Goodbye);
        bytes[0] = b'X';
        assert!(matches!(round_trip_err(&bytes), ProtocolError::BadMagic));

        let mut bytes = encode(&Message::Goodbye);
        bytes[4] = 9;
        assert!(matches!(round_trip_err(&bytes), ProtocolError::Version(9)));

        let mut bytes = encode(&Message::Goodbye);
        bytes[8..12].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(matches!(round_trip_err(&bytes), ProtocolError::TooLarge(_)));
    }

    fn round_trip_err(bytes: &[u8]) -> ProtocolError {
        let mut r = bytes;
        read_message(&mut r, &mut Vec::new()).unwrap_err()
    }

    #[test]
    fn rejects_bad_formats() {
        for (w, h, fps) in [
            (0, 2, 30),
            (3, 2, 30),
            (4, 2, 0),
            (8192, 2, 30),
            (4, 2, 500),
        ] {
            let msg = encode(&Message::SetFormat(VideoFormat {
                width: w,
                height: h,
                fps,
                pixel_format: PixelFormat::Nv12,
            }));
            assert!(
                matches!(round_trip_err(&msg), ProtocolError::Invalid(_)),
                "{w}x{h}@{fps}"
            );
        }
        let mut msg = encode(&Message::SetFormat(FMT));
        msg[HEADER_LEN + 12] = 9; // pixel format code
        assert!(matches!(round_trip_err(&msg), ProtocolError::Invalid(_)));
    }

    #[test]
    fn unknown_kinds_are_skipped() {
        let mut bytes = encode(&Message::Goodbye);
        bytes[6] = 99;
        assert_eq!(round_trip(&bytes), Message::Unknown(99));
    }

    #[test]
    fn truncated_input_is_an_io_error() {
        let bytes = encode(&Message::Hello {
            pid: 1,
            format: FMT,
        });
        assert!(matches!(
            round_trip_err(&bytes[..bytes.len() - 1]),
            ProtocolError::Io(_)
        ));
    }

    /// Cheap fuzzing: random bytes after a valid header never panic.
    #[test]
    fn random_bodies_never_panic() {
        let mut seed = 0x2545_f491_4f6c_dd1du64;
        let mut next = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        for _ in 0..20_000 {
            let kind = (next() % 7) as u16;
            let len = (next() % 64) as usize;
            let body: Vec<u8> = (0..len).map(|_| next() as u8).collect();
            let _ = parse_body(kind, &body);
        }
    }
}
