//! Pipeline errors, classified so the UI can show a friendly message for each kind.

use std::fmt;

/// What went wrong, in terms a user can act on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ErrorKind {
    /// The stream settings can't be used (bad host, unsupported protocol, ...).
    InvalidConfig,
    /// The server rejected the user name or password (401/403).
    Unauthorized,
    /// The server doesn't know the path (404).
    NotFound,
    /// Couldn't open a connection to the host.
    Unreachable,
    /// The server didn't answer in time.
    Timeout,
    /// The stream has no video track.
    NoVideo,
    /// The video codec isn't one RTSP Cam can decode.
    UnsupportedCodec,
    /// The codec is known but no decoder is installed for it (for example HEVC without the
    /// "HEVC Video Extensions").
    DecoderUnavailable,
    /// The decoder failed on the stream's data.
    Decode,
    /// Connected, but no pictures arrived for too long.
    Stalled,
    /// The server ended the stream.
    EndOfStream,
    /// Any other RTSP/RTP protocol failure.
    Protocol,
}

impl ErrorKind {
    /// A short hint for the user, if there is something specific to suggest.
    pub fn hint(self) -> Option<&'static str> {
        Some(match self {
            Self::Unauthorized => "Check the user name and password.",
            Self::NotFound => {
                "Check the path. Camera brands use different paths for their streams."
            }
            Self::Unreachable => {
                "Check the IP address and port, and that the camera is on and on the same network."
            }
            Self::Timeout => "The camera didn't respond in time. Check the address and network.",
            Self::UnsupportedCodec => "Set the camera to H.264, H.265 or MJPEG.",
            Self::DecoderUnavailable => {
                "For H.265 install \"HEVC Video Extensions\" from the Microsoft Store, \
                 or switch the camera (or its sub stream) to H.264."
            }
            Self::Stalled => "The camera stopped sending video.",
            _ => return None,
        })
    }
}

/// An error from connecting, streaming or decoding.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub struct PipelineError {
    kind: ErrorKind,
    message: String,
}

impl PipelineError {
    pub fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }

    pub fn kind(&self) -> ErrorKind {
        self.kind
    }

    pub fn message(&self) -> &str {
        &self.message
    }

    /// Whether retrying with the same settings could help. Used to pick the backoff delay.
    pub fn is_transient(&self) -> bool {
        !matches!(
            self.kind,
            ErrorKind::InvalidConfig
                | ErrorKind::Unauthorized
                | ErrorKind::NotFound
                | ErrorKind::NoVideo
                | ErrorKind::UnsupportedCodec
                | ErrorKind::DecoderUnavailable
        )
    }
}

impl fmt::Display for PipelineError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl From<retina::Error> for PipelineError {
    fn from(e: retina::Error) -> Self {
        // retina's messages carry multi-line connection context. Keep the first line for
        // display; the whole thing is logged where the error happens.
        let full = e.to_string();
        let first = full.lines().next().unwrap_or_default().trim().to_owned();
        let kind = match e.status_code() {
            Some(401 | 403) => ErrorKind::Unauthorized,
            Some(404) => ErrorKind::NotFound,
            Some(_) => ErrorKind::Protocol,
            None if first.starts_with("Unable to connect") => ErrorKind::Unreachable,
            None => ErrorKind::Protocol,
        };
        tracing::debug!(error = %full, ?kind, "RTSP error");
        Self::new(kind, first)
    }
}

#[cfg(windows)]
impl From<windows_core::Error> for PipelineError {
    fn from(e: windows_core::Error) -> Self {
        Self::new(ErrorKind::Decode, format!("Media Foundation: {e}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transient_kinds() {
        assert!(PipelineError::new(ErrorKind::Unreachable, "x").is_transient());
        assert!(PipelineError::new(ErrorKind::Stalled, "x").is_transient());
        assert!(!PipelineError::new(ErrorKind::Unauthorized, "x").is_transient());
        assert!(!PipelineError::new(ErrorKind::DecoderUnavailable, "x").is_transient());
    }

    #[test]
    fn display_is_the_message() {
        let e = PipelineError::new(ErrorKind::Timeout, "no answer");
        assert_eq!(e.to_string(), "no answer");
        assert!(e.kind().hint().is_some());
    }
}
