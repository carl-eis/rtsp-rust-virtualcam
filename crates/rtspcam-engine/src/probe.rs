//! "Test connection": connect once with the settings in the form and report what the camera
//! sends, or why it couldn't be reached.

use std::sync::Arc;
use std::time::{Duration, Instant};

use rtspcam_pipeline::{Frame, Pipeline, PipelineOptions, SourceOptions, StreamState};
use tokio::runtime::Handle;

/// How long a test waits for a first picture.
pub const TEST_TIMEOUT: Duration = Duration::from_secs(15);

/// What a connection test found.
#[derive(Debug, Clone)]
pub struct TestOutcome {
    /// Whether it worked.
    pub ok: bool,
    /// A few lines for the dialog.
    pub text: String,
    /// The first picture, for a thumbnail.
    pub frame: Option<Arc<Frame>>,
}

/// Connects once and reports what the camera sends, or why it could not. Blocks for up to
/// `timeout` (plus the time to stop the connection); run it off the UI thread.
pub fn test_connection(source: SourceOptions, runtime: &Handle, timeout: Duration) -> TestOutcome {
    let pipeline = Pipeline::start("test connection", PipelineOptions::new(source), runtime);
    let status = pipeline.status();
    let started = Instant::now();
    let outcome = loop {
        let state = status.borrow().clone();
        match state {
            StreamState::Streaming(stats) => {
                if let Some(frame) = pipeline.frames().latest() {
                    break TestOutcome {
                        ok: true,
                        text: format!(
                            "Connected.\n{} {}x{}, {:.0} fps\n({})",
                            stats.codec, stats.width, stats.height, stats.fps, stats.decoder
                        ),
                        frame: Some(frame),
                    };
                }
            }
            StreamState::Retrying { error, .. } => {
                let hint = error.kind().hint().unwrap_or_default();
                break TestOutcome {
                    ok: false,
                    text: format!("Failed: {error}\n{hint}"),
                    frame: None,
                };
            }
            _ => {}
        }
        if started.elapsed() > timeout {
            break TestOutcome {
                ok: false,
                text: "No picture arrived in time. Check the address, or whether the camera \
                       sends key frames rarely."
                    .to_owned(),
                frame: None,
            };
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    pipeline.stop();
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unreachable_camera_fails_with_a_reason() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        // Nothing listens on port 9.
        let source = SourceOptions::from_url("rtsp://127.0.0.1:9/x").unwrap();
        let outcome = test_connection(source, rt.handle(), Duration::from_secs(10));
        assert!(!outcome.ok);
        assert!(outcome.text.starts_with("Failed"), "{}", outcome.text);
        assert!(outcome.frame.is_none());
    }
}
