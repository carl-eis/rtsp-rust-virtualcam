/// Errors from listing, opening or running an audio device.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AudioError {
    #[error("no audio device \"{0}\" (unplugged or renamed?)")]
    NotFound(String),
    #[error("this computer has no default {0} device")]
    NoDefault(&'static str),
    #[error("the audio device is in use by another program")]
    Busy,
    #[error("access to the audio device was refused (check the system's privacy settings)")]
    PermissionDenied,
    #[error("the audio device was disconnected")]
    Disconnected,
    #[error("the audio device can't record or play in a usable format: {0}")]
    Unsupported(String),
    #[error("audio error: {0}")]
    Other(String),
}

impl From<cpal::Error> for AudioError {
    fn from(e: cpal::Error) -> Self {
        use cpal::ErrorKind;
        match e.kind() {
            ErrorKind::DeviceNotAvailable => Self::Disconnected,
            ErrorKind::DeviceBusy => Self::Busy,
            ErrorKind::PermissionDenied => Self::PermissionDenied,
            ErrorKind::UnsupportedConfig | ErrorKind::UnsupportedOperation => {
                Self::Unsupported(e.to_string())
            }
            _ => Self::Other(e.to_string()),
        }
    }
}
