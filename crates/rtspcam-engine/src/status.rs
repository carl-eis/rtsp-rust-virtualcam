//! What the UI shows for each camera.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use rtspcam_core::OutputFormat;
use rtspcam_ipc::StreamStatus;
use rtspcam_pipeline::{DEFAULT_LATENCY_CAP, ErrorKind, StreamState, StreamStats};
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

/// A few lines on one stream's latency for the stream details, with the trend of its drift
/// behind live. It is rebuilt at most every [`LatencyReport::EVERY`], so the figures stay
/// readable, and it only uses the stats the pipeline already reports once a second.
#[derive(Debug, Default)]
pub struct LatencyReport {
    stream: Option<Uuid>,
    /// Drift samples over the last [`Self::SPAN`].
    drift: VecDeque<(Instant, Duration)>,
    built: Option<Instant>,
    lines: Vec<String>,
}

impl LatencyReport {
    /// How often the report changes.
    pub const EVERY: Duration = Duration::from_secs(2);
    /// How far back the trend looks.
    const SPAN: Duration = Duration::from_secs(30);
    /// Enough history to call a trend.
    const TREND_AFTER: Duration = Duration::from_secs(10);
    /// A change in drift smaller than this is "steady".
    const TREND_STEP: Duration = Duration::from_millis(50);

    /// The report for `stream`, given its stats (`None` when it isn't streaming) at `now`.
    pub fn update(&mut self, stream: Uuid, stats: Option<&StreamStats>, now: Instant) -> &[String] {
        let Some(stats) = stats else {
            *self = Self::default();
            return &self.lines;
        };
        if self.stream != Some(stream) {
            *self = Self {
                stream: Some(stream),
                ..Self::default()
            };
        }
        if self
            .built
            .is_some_and(|t| now.saturating_duration_since(t) < Self::EVERY)
        {
            return &self.lines;
        }
        self.built = Some(now);
        self.drift.push_back((now, stats.drift));
        while self
            .drift
            .front()
            .is_some_and(|(t, _)| now.saturating_duration_since(*t) > Self::SPAN)
        {
            self.drift.pop_front();
        }
        self.lines = self.build(stats, now);
        &self.lines
    }

    fn build(&self, stats: &StreamStats, now: Instant) -> Vec<String> {
        let ms = |d: Duration| d.as_millis();
        let (min, max) = self
            .drift
            .iter()
            .fold((Duration::MAX, Duration::ZERO), |(lo, hi), &(_, d)| {
                (lo.min(d), hi.max(d))
            });
        let span = self
            .drift
            .front()
            .map_or(Duration::ZERO, |(t, _)| now.saturating_duration_since(*t));
        vec![
            "Latency".to_owned(),
            format!(
                "Arrival to picture: {} ms (decoder {:.1} ms)",
                ms(stats.latency),
                stats.decode_time.as_secs_f64() * 1000.0
            ),
            format!(
                "Behind live: {} ms, {} ({}-{} ms in {} s)",
                ms(stats.drift),
                self.trend(span),
                ms(min),
                ms(max),
                span.as_secs().max(1)
            ),
            format!(
                "Catch-up past {} ms: {} skipped, {} resyncs",
                ms(DEFAULT_LATENCY_CAP),
                stats.frames_skipped,
                stats.resyncs
            ),
        ]
    }

    /// "rising", "falling" or "steady", comparing the oldest and newest third of the samples.
    fn trend(&self, span: Duration) -> &'static str {
        if span < Self::TREND_AFTER {
            return "measuring";
        }
        let n = self.drift.len();
        let third = (n / 3).max(1);
        let average = |skip: usize| {
            self.drift
                .iter()
                .skip(skip)
                .take(third)
                .map(|(_, d)| *d)
                .sum::<Duration>()
                / u32::try_from(third).unwrap_or(u32::MAX)
        };
        let (old, new) = (average(0), average(n - third));
        if new > old + Self::TREND_STEP {
            "rising"
        } else if old > new + Self::TREND_STEP {
            "falling"
        } else {
            "steady"
        }
    }
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

    fn stats(drift_ms: u64) -> StreamStats {
        StreamStats {
            codec: rtspcam_pipeline::VideoCodec::H264,
            decoder: String::new(),
            width: 1280,
            height: 720,
            decoded_size: (1280, 720),
            fps: 30.0,
            bitrate: 0,
            latency: Duration::from_millis(12),
            decode_time: Duration::from_micros(3100),
            drift: Duration::from_millis(drift_ms),
            frames_decoded: 0,
            decode_errors: 0,
            packets_lost: 0,
            frames_skipped: 2,
            resyncs: 0,
        }
    }

    /// Feeds one drift value every [`LatencyReport::EVERY`] and returns the last report.
    fn run(report: &mut LatencyReport, id: Uuid, start: Instant, drifts: &[u64]) -> Vec<String> {
        let mut lines = Vec::new();
        for (i, &d) in drifts.iter().enumerate() {
            let now = start + LatencyReport::EVERY * i as u32;
            lines = report.update(id, Some(&stats(d)), now).to_vec();
        }
        lines
    }

    #[test]
    fn latency_report_lines() {
        let mut report = LatencyReport::default();
        let lines = run(&mut report, Uuid::new_v4(), Instant::now(), &[40]);
        assert_eq!(
            lines,
            [
                "Latency",
                "Arrival to picture: 12 ms (decoder 3.1 ms)",
                "Behind live: 40 ms, measuring (40-40 ms in 1 s)",
                "Catch-up past 350 ms: 2 skipped, 0 resyncs",
            ]
        );
    }

    #[test]
    fn latency_report_changes_at_most_every_two_seconds() {
        let mut report = LatencyReport::default();
        let id = Uuid::new_v4();
        let start = Instant::now();
        let first = report.update(id, Some(&stats(40)), start).to_vec();
        let soon = report
            .update(id, Some(&stats(90)), start + Duration::from_secs(1))
            .to_vec();
        assert_eq!(first, soon);
        let later = report
            .update(id, Some(&stats(90)), start + LatencyReport::EVERY)
            .to_vec();
        assert!(later[2].contains("90 ms"), "{later:?}");
    }

    #[test]
    fn latency_report_trend() {
        let id = Uuid::new_v4();
        let start = Instant::now();
        let rising: Vec<u64> = (0..10).map(|i| i * 40).collect();
        let lines = run(&mut LatencyReport::default(), id, start, &rising);
        assert!(lines[2].contains("rising"), "{lines:?}");
        let lines = run(&mut LatencyReport::default(), id, start, &[40; 10]);
        assert!(lines[2].contains("steady"), "{lines:?}");
    }

    #[test]
    fn latency_report_starts_over_for_another_stream_or_when_not_streaming() {
        let mut report = LatencyReport::default();
        let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
        let start = Instant::now();
        run(&mut report, a, start, &[300; 10]);
        let lines = report.update(b, Some(&stats(10)), start + Duration::from_secs(30));
        assert!(lines[2].contains("measuring (10-10 ms"), "{lines:?}");
        assert!(report.update(b, None, start).is_empty());
    }
}
