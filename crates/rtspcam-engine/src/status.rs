//! What the UI shows for each camera.

use rtspcam_core::OutputFormat;
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

/// Whether the virtual camera exists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VcamState {
    /// Being created.
    Pending,
    /// Visible to Discord, the Camera app and so on.
    Ready,
    /// This OS has no virtual camera support yet; the message says so. Not a problem: the
    /// stream still previews.
    Unsupported(String),
    /// The OS refused it; the message says why (not registered, access denied, ...).
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
    /// The output the camera lists first (width, height, fps). For a "Source" stream, the
    /// camera's picture size as last seen.
    pub offered: (u32, u32, u32),
}

impl CameraStatus {
    /// Whether the stream or its camera has a problem the user should see.
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

/// The output part of a stream's details, for example "1280x720 @ 30 fps", or for a "Source"
/// stream "the camera's own size (2560x1440) @ 60 fps". `offered` is the running camera's
/// [`CameraStatus::offered`], if it has one.
pub fn output_summary(output: &OutputFormat, offered: Option<(u32, u32, u32)>) -> String {
    if output.source {
        let (w, h) = offered
            .map(|(w, h, _)| (w, h))
            .unwrap_or(OutputFormat::SOURCE_FALLBACK);
        format!("the camera's own size ({w}x{h}) @ {} fps", output.fps)
    } else {
        format!("{}x{} @ {} fps", output.width, output.height, output.fps)
    }
}

/// A note for when the camera sends noticeably fewer pictures per second (`camera_fps`, as
/// measured) than the virtual camera's `output_fps`: apps then get some pictures twice, which
/// a viewer can't tell from a stutter.
pub fn frame_rate_note(camera_fps: f32, output_fps: u32) -> Option<String> {
    // Measured once a second, so allow for a frame or so of slack.
    (camera_fps > 0.0 && camera_fps < output_fps as f32 * 0.9).then(|| {
        format!(
            "The camera sends {camera_fps:.1} fps, fewer than the {output_fps} fps output: \
             apps see some pictures twice."
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_lines() {
        let fixed = OutputFormat::default();
        assert_eq!(output_summary(&fixed, None), "1280x720 @ 30 fps");
        assert_eq!(output_summary(&fixed, Some((1, 1, 1))), "1280x720 @ 30 fps");
        let source = OutputFormat::source(60);
        assert_eq!(
            output_summary(&source, Some((2560, 1440, 60))),
            "the camera's own size (2560x1440) @ 60 fps"
        );
        assert_eq!(
            output_summary(&source, None),
            "the camera's own size (1920x1080) @ 60 fps"
        );
    }

    #[test]
    fn repeated_pictures_are_pointed_out() {
        assert!(frame_rate_note(25.0, 60).unwrap().contains("25.0 fps"));
        assert!(frame_rate_note(29.0, 60).is_some());
        // Close enough: a frame more or less in a one-second measurement.
        assert_eq!(frame_rate_note(59.0, 60), None);
        assert_eq!(frame_rate_note(28.5, 30), None);
        assert_eq!(frame_rate_note(30.0, 15), None);
        // Nothing measured yet.
        assert_eq!(frame_rate_note(0.0, 60), None);
    }
}
