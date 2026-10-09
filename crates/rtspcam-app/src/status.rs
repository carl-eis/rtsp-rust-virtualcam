//! What the UI shows for each camera.

use rtspcam_ipc::StreamStatus;
use rtspcam_pipeline::{ErrorKind, StreamState};
use uuid::Uuid;

/// What a camera's pipeline is doing.
#[derive(Debug, Clone, PartialEq)]
pub enum Activity {
    /// On-demand camera nobody is using: the RTSP connection is closed.
    Idle,
    /// "Pause all" is in effect: the connection is closed and consumers see a placeholder.
    Paused,
    /// The stream's configuration can't be used (for example an unsupported protocol).
    Invalid(String),
    /// The pipeline is running; see its state.
    Running(StreamState),
}

impl Activity {
    pub fn is_streaming(&self) -> bool {
        matches!(self, Self::Running(s) if s.is_streaming())
    }

    pub fn is_error(&self) -> bool {
        matches!(
            self,
            Self::Invalid(_) | Self::Running(StreamState::Retrying { .. })
        )
    }

    /// One line for the list view's Status column.
    pub fn summary(&self) -> String {
        match self {
            Self::Idle => "Idle (connects when an app uses it)".to_owned(),
            Self::Paused => "Paused".to_owned(),
            Self::Invalid(why) => format!("Error: {why}"),
            Self::Running(state) => state.summary(),
        }
    }

    /// A hint about what the user can do, if the pipeline failed in a way they can fix.
    pub fn hint(&self) -> Option<String> {
        match self {
            Self::Running(StreamState::Retrying { error, .. }) => {
                error.kind().hint().map(str::to_owned)
            }
            _ => None,
        }
    }

    /// How the media source should describe the camera when it has no picture.
    pub(crate) fn ipc_status(&self) -> (StreamStatus, String) {
        match self {
            Self::Paused => (StreamStatus::Disabled, String::new()),
            // The first consumer is about to start the pipeline.
            Self::Idle => (StreamStatus::Connecting, String::new()),
            Self::Invalid(why) => (StreamStatus::Error, why.clone()),
            Self::Running(state) => match state {
                StreamState::Streaming(_) => (StreamStatus::Streaming, String::new()),
                StreamState::Idle | StreamState::Connecting { .. } => {
                    (StreamStatus::Connecting, String::new())
                }
                StreamState::Retrying { error, .. } => (StreamStatus::Error, error.to_string()),
            },
        }
    }
}

/// Whether the camera exists in Windows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VcamState {
    /// Being created.
    Pending,
    /// Visible to Discord, the Camera app and so on.
    Ready,
    /// Windows refused it; the message says why (not registered, access denied, ...).
    Failed(String),
}

/// A snapshot of one camera.
#[derive(Debug, Clone, PartialEq)]
pub struct CameraStatus {
    pub id: Uuid,
    pub name: String,
    pub activity: Activity,
    pub vcam: VcamState,
    /// Apps currently using the webcam.
    pub clients: usize,
    /// Open preview windows.
    pub previews: usize,
}

impl CameraStatus {
    /// A camera that exists in Windows, or is a stream with a problem the user should see.
    pub fn has_problem(&self) -> bool {
        self.activity.is_error() || matches!(self.vcam, VcamState::Failed(_))
    }

    /// The error classification of a failing pipeline, if there is one.
    pub fn error_kind(&self) -> Option<ErrorKind> {
        match &self.activity {
            Activity::Running(StreamState::Retrying { error, .. }) => Some(error.kind()),
            _ => None,
        }
    }
}
