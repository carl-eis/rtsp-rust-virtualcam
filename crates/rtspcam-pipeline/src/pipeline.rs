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

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use rtspcam_core::profile::{self, Stage};
use tokio::runtime::Handle;
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;

use crate::decode::{self, Decoder, DecoderChoice, create_decoder_with};
use crate::error::{ErrorKind, PipelineError};
use crate::status::{StreamState, StreamStats};
use crate::{EncodedFrame, Frame, FrameBus, RtspSource, SourceOptions, VideoCodec};

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
        /// The decoder's time for this picture.
        decode_time: Duration,
    },
    /// One frame failed; decoding resumes at the next key frame.
    Error(PipelineError),
    /// Decoding can't continue for this session (no decoder for the codec).
    Fatal(PipelineError),
}

/// The largest picture consumers need, shared with the decode thread. 0 means no limit,
/// otherwise width << 32 | height.
#[derive(Debug, Clone, Default)]
struct SizeLimit(Arc<AtomicU64>);

impl SizeLimit {
    fn set(&self, max: Option<(u32, u32)>) {
        let v = max.map_or(0, |(w, h)| (u64::from(w) << 32) | u64::from(h));
        self.0.store(v, Ordering::Relaxed);
    }

    fn get(&self) -> Option<(u32, u32)> {
        match self.0.load(Ordering::Relaxed) {
            0 => None,
            v => Some(((v >> 32) as u32, v as u32)),
        }
    }
}

/// A running stream. Dropping it (or [`stop`](Self::stop)) ends the session and the decode
/// thread.
#[derive(Debug)]
pub struct Pipeline {
    name: String,
    frames: FrameBus,
    status: watch::Receiver<StreamState>,
    limit: SizeLimit,
    task: JoinHandle<()>,
    decode_thread: Option<thread::JoinHandle<()>>,
}

impl Pipeline {
    /// Starts connecting in the background. `name` is used in logs and thread names.
    pub fn start(name: impl Into<String>, opts: PipelineOptions, runtime: &Handle) -> Self {
        let choice = opts.decoder;
        Self::start_with(name, opts, runtime, move |codec, size, hardware| {
            create_decoder_with(codec, size, choice, hardware)
        })
    }

    /// [`start`](Self::start) with a decoder factory of its own (for tests).
    fn start_with(
        name: impl Into<String>,
        opts: PipelineOptions,
        runtime: &Handle,
        create: impl FnMut(VideoCodec, Option<(u32, u32)>, bool) -> DecoderResult + Send + 'static,
    ) -> Self {
        let name = name.into();
        let frames = FrameBus::new();
        let (status_tx, status_rx) = watch::channel(StreamState::Connecting { attempt: 1 });
        let (msg_tx, msg_rx) = mpsc::channel(8);
        let (event_tx, event_rx) = mpsc::unbounded_channel();
        let limit = SizeLimit::default();

        let bus = frames.clone();
        let thread_name = name.clone();
        let thread_limit = limit.clone();
        let decode_thread = thread::Builder::new()
            .name(format!("decode {name}"))
            .spawn(move || {
                DecodeLoop::new(&thread_name, event_tx, bus, thread_limit, create).run(msg_rx);
            })
            .expect("failed to spawn decode thread");

        let task_name = name.clone();
        let task = runtime.spawn(async move {
            ingest_loop(&task_name, opts, msg_tx, event_rx, status_tx).await;
        });

        Self {
            name,
            frames,
            status: status_rx,
            limit,
            task,
            decode_thread: Some(decode_thread),
        }
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    /// The largest picture any consumer needs (see [`Decoder::limit_size`]), so a decoder that
    /// can shrink pictures cheaply does it before the rest of the pipeline sees them. `None`
    /// (the default) means full size. Takes effect with the next picture.
    pub fn set_size_limit(&self, max: Option<(u32, u32)>) {
        self.limit.set(max);
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
                DecodeEvent::Picture { width, height, latency, decode_time } => {
                    stats.picture(width, height, latency, decode_time);
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
    decode_sum: Duration,
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
                decode_time: Duration::ZERO,
                frames_decoded: 0,
                decode_errors: 0,
                packets_lost: 0,
            },
            window_start: Instant::now(),
            pictures: 0,
            bytes: 0,
            latency_sum: Duration::ZERO,
            decode_sum: Duration::ZERO,
            last_picture: None,
        }
    }

    fn streamed(&self) -> bool {
        self.last_picture.is_some()
    }

    fn picture(&mut self, width: u32, height: u32, latency: Duration, decode_time: Duration) {
        self.totals.width = width;
        self.totals.height = height;
        self.totals.frames_decoded += 1;
        self.pictures += 1;
        self.latency_sum += latency;
        self.decode_sum += decode_time;
        self.last_picture = Some(Instant::now());
    }

    /// Stats for the window since the last snapshot, and starts a new window.
    fn snapshot(&mut self) -> StreamStats {
        let secs = self.window_start.elapsed().as_secs_f64().max(0.001);
        self.totals.fps = (f64::from(self.pictures) / secs) as f32;
        self.totals.bitrate = (self.bytes as f64 * 8.0 / secs) as u64;
        if self.pictures > 0 {
            self.totals.latency = self.latency_sum / self.pictures;
            self.totals.decode_time = self.decode_sum / self.pictures;
        }
        self.window_start = Instant::now();
        self.pictures = 0;
        self.bytes = 0;
        self.latency_sum = Duration::ZERO;
        self.decode_sum = Duration::ZERO;
        self.totals.clone()
    }
}

type DecoderResult = Result<Box<dyn Decoder>, PipelineError>;

/// After this many lost decoders in one session, the session stays on software decoding.
const MAX_LOST_DECODERS: u32 = 3;

/// The decoder in use and what is known about it.
struct Current {
    decoder: Box<dyn Decoder>,
    /// The [`decode::generation`] it was created in.
    generation: u64,
    /// The name last reported (hardware decoders may rename themselves after the first
    /// picture, when they know whether the GPU decodes).
    reported: String,
    /// The size limit last passed to it.
    limit: Option<(u32, u32)>,
    produced: bool,
}

/// The decode thread: owns the decoder (platform decoder objects stay on this thread).
///
/// The decoder is created at the first key frame of a session and replaced, without
/// reconnecting, when
/// - it reports [`ErrorKind::DecoderLost`]: dropped at once, a new one at the next key frame.
///   If it never produced a picture (hardware decoding doesn't work for this stream), the
///   rest of the session decodes in software;
/// - hardware decoding is switched on or off ([`decode::generation`] changed): the old one
///   keeps decoding until the next key frame, then the new one takes over, so the picture
///   never stops.
struct DecodeLoop<'a, F> {
    name: &'a str,
    events: mpsc::UnboundedSender<DecodeEvent>,
    bus: FrameBus,
    limit: SizeLimit,
    create: F,
    current: Option<Current>,
    session: Option<(VideoCodec, Option<(u32, u32)>)>,
    /// No decoder for this session; frames are dropped until the next one.
    failed: bool,
    wait_for_key: bool,
    /// Hardware decoding failed for this session.
    software_only: bool,
    lost: u32,
    pictures: Vec<Frame>,
}

impl<'a, F> DecodeLoop<'a, F>
where
    F: FnMut(VideoCodec, Option<(u32, u32)>, bool) -> DecoderResult,
{
    fn new(
        name: &'a str,
        events: mpsc::UnboundedSender<DecodeEvent>,
        bus: FrameBus,
        limit: SizeLimit,
        create: F,
    ) -> Self {
        Self {
            name,
            events,
            bus,
            limit,
            create,
            current: None,
            session: None,
            failed: false,
            wait_for_key: true,
            software_only: false,
            lost: 0,
            pictures: Vec::new(),
        }
    }

    fn run(mut self, mut rx: mpsc::Receiver<DecodeMsg>) {
        while let Some(msg) = rx.blocking_recv() {
            match msg {
                DecodeMsg::Start { codec, size } => {
                    self.current = None;
                    self.session = Some((codec, size));
                    self.failed = false;
                    self.wait_for_key = true;
                    self.software_only = false;
                    self.lost = 0;
                }
                DecodeMsg::Frame(frame) => self.frame(&frame),
            }
        }
    }

    fn frame(&mut self, frame: &EncodedFrame) {
        if self.failed || (self.wait_for_key && !frame.keyframe) {
            return;
        }
        self.wait_for_key = false;
        if frame.keyframe
            && let Some(c) = &self.current
            && c.generation != decode::generation()
        {
            tracing::info!(
                stream = self.name,
                "hardware decoding switched; new decoder"
            );
            self.current = None;
        }
        if self.current.is_none() && !self.start_decoder() {
            return;
        }
        let Some(c) = self.current.as_mut() else {
            return;
        };
        let limit = self.limit.get();
        if c.limit != limit {
            c.decoder.limit_size(limit);
            c.limit = limit;
        }
        let started = Instant::now();
        let result = c.decoder.decode(frame, &mut self.pictures);
        let took = started.elapsed();
        profile::record(Stage::Decode, took);

        if c.decoder.name() != c.reported {
            c.reported = c.decoder.name().to_owned();
            let _ = self.events.send(DecodeEvent::Ready {
                decoder: c.reported.clone(),
            });
        }
        c.produced |= !self.pictures.is_empty();
        let produced = c.produced;
        match result {
            Ok(()) => {}
            Err(e) if e.kind() == ErrorKind::DecoderLost => {
                self.lost += 1;
                if !produced || self.lost >= MAX_LOST_DECODERS {
                    self.software_only = true;
                }
                tracing::warn!(
                    stream = self.name,
                    error = %e,
                    software_only = self.software_only,
                    "decoder lost; replacing it at the next key frame"
                );
                self.current = None;
                self.wait_for_key = true;
            }
            Err(e) => {
                // Skip to the next key frame; the decoder's references are now suspect.
                self.wait_for_key = true;
                let _ = self.events.send(DecodeEvent::Error(e));
            }
        }
        let count = u32::try_from(self.pictures.len())
            .unwrap_or(u32::MAX)
            .max(1);
        for picture in self.pictures.drain(..) {
            let _ = self.events.send(DecodeEvent::Picture {
                width: picture.width(),
                height: picture.height(),
                latency: picture.decode_latency(),
                decode_time: took / count,
            });
            self.bus.publish(picture);
        }
    }

    /// Creates the session's decoder. `false` if there is none (the session has failed).
    fn start_decoder(&mut self) -> bool {
        let Some((codec, size)) = self.session else {
            return false;
        };
        let generation = decode::generation();
        match (self.create)(codec, size, !self.software_only) {
            Ok(decoder) => {
                let reported = decoder.name().to_owned();
                let _ = self.events.send(DecodeEvent::Ready {
                    decoder: reported.clone(),
                });
                self.current = Some(Current {
                    decoder,
                    generation,
                    reported,
                    limit: None,
                    produced: false,
                });
                true
            }
            Err(e) => {
                self.failed = true;
                let _ = self.events.send(DecodeEvent::Fatal(e));
                false
            }
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
            w.picture(
                640,
                480,
                Duration::from_millis(10),
                Duration::from_millis(2),
            );
        }
        let s = w.snapshot();
        assert!(w.streamed());
        assert_eq!((s.width, s.height, s.frames_decoded), (640, 480, 3));
        assert_eq!(s.latency, Duration::from_millis(10));
        assert!(s.fps > 0.0 && s.bitrate > 0);
        assert_eq!(w.pictures, 0);
    }

    type Limit = Option<(u32, u32)>;

    /// A decoder that makes one 4x2 picture per frame, or fails as told.
    struct FakeDecoder {
        name: String,
        lose_at: Option<usize>,
        decoded: usize,
        limits: Arc<std::sync::Mutex<Vec<Limit>>>,
    }

    impl Decoder for FakeDecoder {
        fn name(&self) -> &str {
            &self.name
        }

        fn decode(&mut self, _: &EncodedFrame, out: &mut Vec<Frame>) -> Result<(), PipelineError> {
            if self.lose_at == Some(self.decoded) {
                return Err(PipelineError::new(ErrorKind::DecoderLost, "device removed"));
            }
            self.decoded += 1;
            if self.decoded == 1 {
                // Like a hardware decoder that knows what it is after the first picture.
                self.name.push_str(" (GPU)");
            }
            out.push(Frame::black(4, 2));
            Ok(())
        }

        fn limit_size(&mut self, max: Option<(u32, u32)>) {
            self.limits.lock().unwrap().push(max);
        }
    }

    /// Runs a decode loop over `frames` (true = key frame). `lose` says, per created decoder,
    /// after how many frames it reports itself lost. Returns the `hardware` argument of every
    /// decoder created, and the events.
    fn run_loop(
        frames: &[bool],
        lose: &[Option<usize>],
        between: impl Fn(usize),
    ) -> (Vec<bool>, Vec<DecodeEvent>, Vec<Limit>) {
        let (events_tx, mut events_rx) = mpsc::unbounded_channel();
        let created = Arc::new(std::sync::Mutex::new(Vec::new()));
        let limits = Arc::new(std::sync::Mutex::new(Vec::new()));
        let limit = SizeLimit::default();
        limit.set(Some((2, 2)));
        let (c, l, lose) = (created.clone(), limits.clone(), lose.to_vec());
        let mut lp = DecodeLoop::new(
            "test",
            events_tx,
            FrameBus::new(),
            limit,
            move |_, _, hardware: bool| -> DecoderResult {
                let mut c = c.lock().unwrap();
                c.push(hardware);
                Ok(Box::new(FakeDecoder {
                    name: format!("fake {}", c.len()),
                    lose_at: lose.get(c.len() - 1).copied().flatten(),
                    decoded: 0,
                    limits: l.clone(),
                }))
            },
        );
        lp.session = Some((VideoCodec::H264, None));
        for (i, &keyframe) in frames.iter().enumerate() {
            between(i);
            lp.frame(&EncodedFrame {
                codec: VideoCodec::H264,
                data: Vec::new(),
                keyframe,
                pts: Duration::ZERO,
                received: Instant::now(),
                loss: 0,
            });
        }
        drop(lp);
        let mut events = Vec::new();
        while let Ok(e) = events_rx.try_recv() {
            events.push(e);
        }
        let created = created.lock().unwrap().clone();
        let limits = limits.lock().unwrap().clone();
        (created, events, limits)
    }

    fn pictures(events: &[DecodeEvent]) -> usize {
        events
            .iter()
            .filter(|e| matches!(e, DecodeEvent::Picture { .. }))
            .count()
    }

    fn names(events: &[DecodeEvent]) -> Vec<String> {
        events
            .iter()
            .filter_map(|e| match e {
                DecodeEvent::Ready { decoder } => Some(decoder.clone()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn a_decoder_lost_before_its_first_picture_is_replaced_in_software() {
        // Key frames at 0 and 3. The first decoder fails at once.
        let (created, events, _) =
            run_loop(&[true, false, false, true, false], &[Some(0), None], |_| {});
        assert_eq!(created, [true, false], "the second decoder is software");
        assert_eq!(
            pictures(&events),
            2,
            "pictures again from the next key frame"
        );
        assert!(
            !events.iter().any(|e| matches!(e, DecodeEvent::Fatal(_))),
            "the session goes on"
        );
    }

    #[test]
    fn a_decoder_lost_after_pictures_may_use_hardware_again() {
        let (created, events, _) = run_loop(&[true, false, true, false], &[Some(1), None], |_| {});
        assert_eq!(created, [true, true]);
        assert_eq!(pictures(&events), 3);
    }

    #[test]
    fn renames_and_size_limits_reach_the_decoder() {
        let (_, events, limits) = run_loop(&[true, false], &[None], |_| {});
        assert_eq!(names(&events), ["fake 1", "fake 1 (GPU)"]);
        assert_eq!(limits, [Some((2, 2))], "passed once, not per frame");
    }

    #[test]
    fn switching_hardware_replaces_the_decoder_at_the_next_key_frame() {
        let _lock = decode::HARDWARE_SWITCH
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let (created, events, _) =
            run_loop(&[true, false, false, true, false], &[None, None], |i| {
                if i == 1 {
                    decode::set_use_hardware(!decode::uses_hardware());
                }
            });
        assert!(created.len() >= 2, "{created:?}");
        assert_eq!(
            pictures(&events),
            5,
            "the old decoder keeps going until the key frame"
        );
    }
}
