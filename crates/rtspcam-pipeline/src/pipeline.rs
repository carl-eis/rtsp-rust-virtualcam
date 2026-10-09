//! A running stream: RTSP ingest on a tokio task, decoding on a dedicated thread, pictures on a
//! [`FrameBus`], and reconnects with exponential backoff.
//!
//! ```text
//!  tokio task                          decode thread
//!  RtspSource ──EncodedFrame──► (bounded) ──► Decoder ──► FrameBus
//!      ▲  │                                      │
//!      │  └──── StreamState (watch) ◄── events ◄─┘
//!   backoff / stall detection
//! ```
//!
//! The bounded channel makes a slow decoder push back on the network read instead of
//! dropping compressed frames (which would corrupt the picture until the next key frame).

use std::thread;
use std::time::{Duration, Instant};

use tokio::runtime::Handle;
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;

use crate::decode::{DecoderChoice, create_decoder};
use crate::error::{ErrorKind, PipelineError};
use crate::status::{StreamState, StreamStats};
use crate::{EncodedFrame, FrameBus, RtspSource, SourceOptions, VideoCodec};

/// Reconnect delays: `initial`, doubling up to `max`, each randomized by ±`jitter`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BackoffConfig {
    pub initial: Duration,
    pub max: Duration,
    /// Fraction (0.0-1.0) of random variation, so many cameras don't retry in lockstep.
    pub jitter: f64,
}

impl Default for BackoffConfig {
    fn default() -> Self {
        Self {
            initial: Duration::from_secs(1),
            max: Duration::from_secs(30),
            jitter: 0.2,
        }
    }
}

#[derive(Debug)]
struct Backoff {
    config: BackoffConfig,
    next: Duration,
}

impl Backoff {
    fn new(config: BackoffConfig) -> Self {
        Self {
            config,
            next: config.initial,
        }
    }

    fn reset(&mut self) {
        self.next = self.config.initial;
    }

    /// The delay before the next attempt. Errors that retrying won't fix wait the maximum.
    fn delay(&mut self, error: &PipelineError) -> Duration {
        let base = if error.is_transient() {
            let d = self.next;
            self.next = (self.next * 2).min(self.config.max);
            d
        } else {
            self.config.max
        };
        let j = self.config.jitter.clamp(0.0, 1.0);
        if j == 0.0 {
            return base;
        }
        base.mul_f64(rand::random_range(1.0 - j..=1.0 + j))
    }
}

/// How a pipeline connects, decodes and recovers.
#[derive(Debug, Clone)]
pub struct PipelineOptions {
    pub source: SourceOptions,
    pub decoder: DecoderChoice,
    /// Reconnect when no picture has been decoded for this long while streaming.
    pub stall_timeout: Duration,
    /// Reconnect when the first picture of a session hasn't arrived after this long. Longer
    /// than `stall_timeout`, because some cameras send a key frame only every few seconds.
    pub first_picture_timeout: Duration,
    pub backoff: BackoffConfig,
}

impl PipelineOptions {
    pub fn new(source: SourceOptions) -> Self {
        Self {
            source,
            decoder: DecoderChoice::default(),
            stall_timeout: Duration::from_secs(5),
            first_picture_timeout: Duration::from_secs(15),
            backoff: BackoffConfig::default(),
        }
    }
}

enum DecodeMsg {
    /// A new session: drop the old decoder and wait for a key frame.
    Start {
        codec: VideoCodec,
        size: Option<(u32, u32)>,
    },
    Frame(EncodedFrame),
}

enum DecodeEvent {
    Ready {
        decoder: String,
    },
    Picture {
        width: u32,
        height: u32,
        latency: Duration,
    },
    /// One frame failed; decoding resumes at the next key frame.
    Error(PipelineError),
    /// Decoding can't continue for this session (no decoder for the codec).
    Fatal(PipelineError),
}

/// A running stream. Dropping it (or [`stop`](Self::stop)) ends the session and the decode
/// thread.
#[derive(Debug)]
pub struct Pipeline {
    name: String,
    frames: FrameBus,
    status: watch::Receiver<StreamState>,
    task: JoinHandle<()>,
    decode_thread: Option<thread::JoinHandle<()>>,
}

impl Pipeline {
    /// Starts connecting in the background. `name` is used in logs and thread names.
    pub fn start(name: impl Into<String>, opts: PipelineOptions, runtime: &Handle) -> Self {
        let name = name.into();
        let frames = FrameBus::new();
        let (status_tx, status_rx) = watch::channel(StreamState::Connecting { attempt: 1 });
        let (msg_tx, msg_rx) = mpsc::channel(8);
        let (event_tx, event_rx) = mpsc::unbounded_channel();

        let bus = frames.clone();
        let choice = opts.decoder;
        let decode_thread = thread::Builder::new()
            .name(format!("decode {name}"))
            .spawn(move || decode_loop(msg_rx, event_tx, bus, choice))
            .expect("failed to spawn decode thread");

        let task_name = name.clone();
        let task = runtime.spawn(async move {
            ingest_loop(&task_name, opts, msg_tx, event_rx, status_tx).await;
        });

        Self {
            name,
            frames,
            status: status_rx,
            task,
            decode_thread: Some(decode_thread),
        }
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    /// Decoded pictures.
    pub fn frames(&self) -> &FrameBus {
        &self.frames
    }

    /// Follows state changes.
    pub fn status(&self) -> watch::Receiver<StreamState> {
        self.status.clone()
    }

    pub fn current_status(&self) -> StreamState {
        self.status.borrow().clone()
    }

    /// Ends the session and waits for the decode thread to finish (normally well under a
    /// second). Don't call from inside an async task; use `spawn_blocking` there.
    pub fn stop(mut self) {
        self.task.abort();
        if let Some(t) = self.decode_thread.take() {
            // The thread exits once the aborted task drops its sender.
            let _ = t.join();
        }
    }
}

impl Drop for Pipeline {
    fn drop(&mut self) {
        // The decode thread winds down by itself once the task is gone.
        self.task.abort();
    }
}

async fn ingest_loop(
    name: &str,
    opts: PipelineOptions,
    msg_tx: mpsc::Sender<DecodeMsg>,
    mut events: mpsc::UnboundedReceiver<DecodeEvent>,
    status: watch::Sender<StreamState>,
) {
    let mut backoff = Backoff::new(opts.backoff);
    let mut attempt = 0;
    loop {
        attempt += 1;
        status.send_replace(StreamState::Connecting { attempt });
        tracing::info!(stream = name, url = %opts.source.url, attempt, "connecting");

        let (error, streamed) = run_session(name, &opts, &msg_tx, &mut events, &status).await;
        if msg_tx.is_closed() {
            tracing::error!(stream = name, "decode thread ended unexpectedly");
            return;
        }
        if streamed {
            backoff.reset();
            attempt = 0;
        }
        let delay = backoff.delay(&error);
        tracing::warn!(
            stream = name,
            error = %error,
            kind = ?error.kind(),
            retry_in = ?delay,
            "stream interrupted"
        );
        status.send_replace(StreamState::Retrying {
            error,
            retry_at: Instant::now() + delay,
            attempt,
        });
        tokio::time::sleep(delay).await;
    }
}

/// One RTSP session from connect to failure. Returns the error that ended it and whether any
/// picture was decoded.
async fn run_session(
    name: &str,
    opts: &PipelineOptions,
    msg_tx: &mpsc::Sender<DecodeMsg>,
    events: &mut mpsc::UnboundedReceiver<DecodeEvent>,
    status: &watch::Sender<StreamState>,
) -> (PipelineError, bool) {
    let mut source = match RtspSource::connect(&opts.source).await {
        Ok(s) => s,
        Err(e) => return (e, false),
    };
    let info = source.info().clone();
    tracing::info!(stream = name, ?info, "playing");

    // Forget events from the previous session, then reset the decoder.
    while events.try_recv().is_ok() {}
    let start = DecodeMsg::Start {
        codec: info.codec,
        size: info.size,
    };
    if msg_tx.send(start).await.is_err() {
        return (decode_thread_gone(), false);
    }

    let started = Instant::now();
    let mut stats = StatsWindow::new(info.codec);
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    loop {
        tokio::select! {
            frame = source.next_frame() => match frame {
                Ok(Some(frame)) => {
                    stats.bytes += frame.data.len() as u64;
                    stats.totals.packets_lost += u64::from(frame.loss);
                    if msg_tx.send(DecodeMsg::Frame(frame)).await.is_err() {
                        return (decode_thread_gone(), stats.streamed());
                    }
                }
                Ok(None) => {
                    let e = PipelineError::new(ErrorKind::EndOfStream, "the server ended the stream");
                    return (e, stats.streamed());
                }
                Err(e) => return (e, stats.streamed()),
            },
            Some(event) = events.recv() => match event {
                DecodeEvent::Ready { decoder } => {
                    tracing::info!(stream = name, decoder, "decoder ready");
                    stats.totals.decoder = decoder;
                }
                DecodeEvent::Picture { width, height, latency } => {
                    stats.picture(width, height, latency);
                }
                DecodeEvent::Error(e) => {
                    stats.totals.decode_errors += 1;
                    tracing::debug!(stream = name, error = %e, "frame not decoded");
                }
                DecodeEvent::Fatal(e) => return (e, stats.streamed()),
            },
            _ = tick.tick() => {
                let (since, limit) = match stats.last_picture {
                    Some(t) => (t, opts.stall_timeout),
                    None => (started, opts.first_picture_timeout),
                };
                if since.elapsed() > limit {
                    let what = if stats.streamed() { "no pictures" } else { "no picture yet" };
                    let e = PipelineError::new(
                        ErrorKind::Stalled,
                        format!("{what} for {} s", limit.as_secs()),
                    );
                    return (e, stats.streamed());
                }
                if stats.streamed() {
                    status.send_replace(StreamState::Streaming(stats.snapshot()));
                }
            }
        }
    }
}

fn decode_thread_gone() -> PipelineError {
    PipelineError::new(ErrorKind::Decode, "the decode thread has stopped")
}

/// Per-second measurements for [`StreamStats`].
struct StatsWindow {
    totals: StreamStats,
    window_start: Instant,
    pictures: u32,
    bytes: u64,
    latency_sum: Duration,
    last_picture: Option<Instant>,
}

impl StatsWindow {
    fn new(codec: VideoCodec) -> Self {
        Self {
            totals: StreamStats {
                codec,
                decoder: String::new(),
                width: 0,
                height: 0,
                fps: 0.0,
                bitrate: 0,
                latency: Duration::ZERO,
                frames_decoded: 0,
                decode_errors: 0,
                packets_lost: 0,
            },
            window_start: Instant::now(),
            pictures: 0,
            bytes: 0,
            latency_sum: Duration::ZERO,
            last_picture: None,
        }
    }

    fn streamed(&self) -> bool {
        self.last_picture.is_some()
    }

    fn picture(&mut self, width: u32, height: u32, latency: Duration) {
        self.totals.width = width;
        self.totals.height = height;
        self.totals.frames_decoded += 1;
        self.pictures += 1;
        self.latency_sum += latency;
        self.last_picture = Some(Instant::now());
    }

    /// Stats for the window since the last snapshot, and starts a new window.
    fn snapshot(&mut self) -> StreamStats {
        let secs = self.window_start.elapsed().as_secs_f64().max(0.001);
        self.totals.fps = (f64::from(self.pictures) / secs) as f32;
        self.totals.bitrate = (self.bytes as f64 * 8.0 / secs) as u64;
        if self.pictures > 0 {
            self.totals.latency = self.latency_sum / self.pictures;
        }
        self.window_start = Instant::now();
        self.pictures = 0;
        self.bytes = 0;
        self.latency_sum = Duration::ZERO;
        self.totals.clone()
    }
}

/// The decode thread: owns the decoder (platform decoder objects stay on this thread).
fn decode_loop(
    mut rx: mpsc::Receiver<DecodeMsg>,
    events: mpsc::UnboundedSender<DecodeEvent>,
    bus: FrameBus,
    choice: DecoderChoice,
) {
    let mut decoder = None;
    let mut session: Option<(VideoCodec, Option<(u32, u32)>)> = None;
    let mut failed = false;
    let mut wait_for_key = true;
    let mut pictures = Vec::new();

    while let Some(msg) = rx.blocking_recv() {
        let frame = match msg {
            DecodeMsg::Start { codec, size } => {
                decoder = None;
                session = Some((codec, size));
                failed = false;
                wait_for_key = true;
                continue;
            }
            DecodeMsg::Frame(frame) => frame,
        };
        if failed || (wait_for_key && !frame.keyframe) {
            continue;
        }
        wait_for_key = false;
        if decoder.is_none() {
            let Some((codec, size)) = session else {
                continue;
            };
            match create_decoder(codec, size, choice) {
                Ok(d) => {
                    let _ = events.send(DecodeEvent::Ready {
                        decoder: d.name().to_owned(),
                    });
                    decoder = Some(d);
                }
                Err(e) => {
                    failed = true;
                    let _ = events.send(DecodeEvent::Fatal(e));
                    continue;
                }
            }
        }
        let Some(d) = decoder.as_mut() else { continue };
        if let Err(e) = d.decode(&frame, &mut pictures) {
            // Skip to the next key frame; the decoder's references are now suspect.
            wait_for_key = true;
            let _ = events.send(DecodeEvent::Error(e));
        }
        for picture in pictures.drain(..) {
            let _ = events.send(DecodeEvent::Picture {
                width: picture.width(),
                height: picture.height(),
                latency: picture.decode_latency(),
            });
            bus.publish(picture);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_doubles_up_to_max_and_resets() {
        let mut b = Backoff::new(BackoffConfig {
            initial: Duration::from_secs(1),
            max: Duration::from_secs(5),
            jitter: 0.0,
        });
        let e = PipelineError::new(ErrorKind::Unreachable, "x");
        let delays: Vec<u64> = (0..5).map(|_| b.delay(&e).as_secs()).collect();
        assert_eq!(delays, [1, 2, 4, 5, 5]);
        b.reset();
        assert_eq!(b.delay(&e).as_secs(), 1);
    }

    #[test]
    fn permanent_errors_wait_the_maximum() {
        let mut b = Backoff::new(BackoffConfig {
            jitter: 0.0,
            ..BackoffConfig::default()
        });
        let e = PipelineError::new(ErrorKind::Unauthorized, "x");
        assert_eq!(b.delay(&e), Duration::from_secs(30));
    }

    #[test]
    fn jitter_stays_in_range() {
        let mut b = Backoff::new(BackoffConfig::default());
        let e = PipelineError::new(ErrorKind::Timeout, "x");
        b.reset();
        for _ in 0..100 {
            b.reset();
            let d = b.delay(&e).as_secs_f64();
            assert!((0.8..=1.2).contains(&d), "{d}");
        }
    }

    #[test]
    fn stats_window() {
        let mut w = StatsWindow::new(VideoCodec::H264);
        assert!(!w.streamed());
        w.bytes = 1000;
        for _ in 0..3 {
            w.picture(640, 480, Duration::from_millis(10));
        }
        let s = w.snapshot();
        assert!(w.streamed());
        assert_eq!((s.width, s.height, s.frames_decoded), (640, 480, 3));
        assert_eq!(s.latency, Duration::from_millis(10));
        assert!(s.fps > 0.0 && s.bitrate > 0);
        assert_eq!(w.pictures, 0);
    }
}
