//! Frame delivery for *push* camera backends, where the app hands each picture to the OS
//! (v4l2loopback on Linux, a CoreMediaIO sink stream on macOS) instead of a consumer asking for
//! it (the Windows DLL).
//!
//! A [`Pusher`] is a thread per camera that, at the camera's frame rate, takes the newest
//! picture from the camera's [`FrameSource`] and writes it to a [`FrameSink`]. It follows the
//! same rules as the Windows DLL's delivery loop (`rtspcam-vcam`'s `stream.rs`), so cameras
//! behave alike on every OS:
//!
//! - Writes are paced to the frame rate; a late wake-up doesn't cause a burst.
//! - The last picture is written again while it is younger than
//!   [`STALE_AFTER`](rtspcam_ipc::placeholder::STALE_AFTER), then the placeholder for the
//!   source's [`status`](FrameSource::status) ("No signal", "Connecting...", ...).
//! - On-demand streams: `client_connected` is called while the sink reports a reader and
//!   `client_disconnected` when it stops (or the pusher stops). Nothing is fetched or written
//!   while there is no reader. A sink that can't tell ([`FrameSink::has_reader`] returns
//!   `None`) counts as one reader for as long as the pusher runs, so an on-demand stream runs
//!   whenever its camera exists.
//!
//! No OS calls here: the backends supply the sink.

use std::fmt;
use std::io;
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use rtspcam_ipc::placeholder::{self, STALE_AFTER};
use rtspcam_ipc::{FrameSource, VideoFormat};

/// Where a [`Pusher`] writes a camera's pictures: an OS video device or buffer queue.
pub trait FrameSink: Send + 'static {
    /// Hands one picture to the device: exactly `format.frame_len()` bytes in the pusher's
    /// format. An error is logged and the next picture is tried at the next tick.
    fn write(&mut self, frame: &[u8]) -> io::Result<()>;

    /// Whether an app is reading the camera right now. `None` (the default): the OS can't tell,
    /// and the pusher treats the camera as always read.
    ///
    /// Called once per frame, so it should be cheap.
    fn has_reader(&mut self) -> Option<bool> {
        None
    }
}

/// A running delivery thread for one camera. Dropping it (or [`stop`](Self::stop)) stops the
/// thread, waits for it, and reports the consumer as gone to the source.
pub struct Pusher {
    name: String,
    stop: Arc<Stop>,
    thread: Option<JoinHandle<()>>,
}

impl fmt::Debug for Pusher {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Pusher")
            .field("name", &self.name)
            .finish_non_exhaustive()
    }
}

impl Pusher {
    /// Starts writing `frames` to `sink` in `format`. `name` is the camera's name, for the
    /// thread name and logs.
    pub fn start(
        name: &str,
        format: VideoFormat,
        frames: Arc<dyn FrameSource>,
        sink: Box<dyn FrameSink>,
    ) -> io::Result<Self> {
        Self::start_with(name, format, frames, sink, STALE_AFTER)
    }

    fn start_with(
        name: &str,
        format: VideoFormat,
        frames: Arc<dyn FrameSource>,
        sink: Box<dyn FrameSink>,
        stale_after: Duration,
    ) -> io::Result<Self> {
        let stop = Arc::new(Stop::default());
        let thread_stop = stop.clone();
        let mut delivery = Delivery {
            name: name.to_owned(),
            format,
            frames,
            sink,
            stale_after,
            frame: Vec::new(),
            scratch: Vec::new(),
            seq: None,
            fresh_at: None,
            placeholder: None,
            connected: false,
            failing: false,
        };
        let thread = std::thread::Builder::new()
            .name(format!("rtspcam push {name}"))
            .spawn(move || delivery.run(&thread_stop))?;
        tracing::debug!(camera = name, %format, "frame pusher started");
        Ok(Self {
            name: name.to_owned(),
            stop,
            thread: Some(thread),
        })
    }

    /// Stops the thread and waits for it.
    pub fn stop(mut self) {
        self.shutdown();
    }

    fn shutdown(&mut self) {
        let Some(thread) = self.thread.take() else {
            return;
        };
        *self
            .stop
            .flag
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = true;
        self.stop.wake.notify_all();
        if thread.join().is_err() {
            tracing::error!(camera = self.name, "the frame pusher panicked");
        }
        tracing::debug!(camera = self.name, "frame pusher stopped");
    }
}

impl Drop for Pusher {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[derive(Default)]
struct Stop {
    flag: Mutex<bool>,
    wake: Condvar,
}

impl Stop {
    /// Waits until `deadline`. Returns `true` if the pusher was stopped instead.
    fn wait_until(&self, deadline: Instant) -> bool {
        let mut stopped = self.flag.lock().unwrap_or_else(PoisonError::into_inner);
        loop {
            if *stopped {
                return true;
            }
            let now = Instant::now();
            if now >= deadline {
                return false;
            }
            stopped = self
                .wake
                .wait_timeout(stopped, deadline - now)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    }
}

/// The thread's state.
struct Delivery {
    name: String,
    format: VideoFormat,
    frames: Arc<dyn FrameSource>,
    sink: Box<dyn FrameSink>,
    stale_after: Duration,
    /// The last picture from the source, and its sequence number.
    frame: Vec<u8>,
    scratch: Vec<u8>,
    seq: Option<u64>,
    /// When `frame` arrived. `None`: no picture since the reader came.
    fresh_at: Option<Instant>,
    /// The placeholder picture and the text it shows.
    placeholder: Option<((String, String), Vec<u8>)>,
    /// `client_connected` was called and not yet matched by `client_disconnected`.
    connected: bool,
    /// The last write failed (logged once until a write succeeds).
    failing: bool,
}

impl Delivery {
    fn run(&mut self, stop: &Stop) {
        let interval = Duration::from_secs(1) / self.format.fps.max(1);
        let mut next_due = Instant::now();
        while !stop.wait_until(next_due) {
            let now = Instant::now();
            next_due = next_due.max(now.checked_sub(interval).unwrap_or(now)) + interval;
            self.tick();
        }
        self.set_connected(false);
    }

    fn tick(&mut self) {
        let reading = self.sink.has_reader().unwrap_or(true);
        self.set_connected(reading);
        if !reading {
            return;
        }
        if let Some(seq) = self
            .frames
            .next_frame(self.format, self.seq, &mut self.scratch)
        {
            std::mem::swap(&mut self.frame, &mut self.scratch);
            self.seq = Some(seq);
            self.fresh_at = Some(Instant::now());
        }
        let live = self
            .fresh_at
            .is_some_and(|at| at.elapsed() < self.stale_after);
        let result = if live {
            self.sink.write(&self.frame)
        } else {
            self.update_placeholder();
            let (_, picture) = self.placeholder.as_ref().expect("placeholder was just set");
            self.sink.write(picture)
        };
        match result {
            Ok(()) if self.failing => {
                self.failing = false;
                tracing::info!(camera = self.name, "writing frames works again");
            }
            Ok(()) => {}
            Err(e) if !self.failing => {
                self.failing = true;
                tracing::warn!(camera = self.name, "could not write a frame: {e}");
            }
            Err(_) => {}
        }
    }

    /// Makes `placeholder` match the source's current status, rendering it again only when
    /// its text changes.
    fn update_placeholder(&mut self) {
        let (status, message) = self.frames.status();
        let (title, detail) = placeholder::text_for_status(status, &message);
        let text = (title.to_owned(), detail.to_owned());
        if self.placeholder.as_ref().is_none_or(|(t, _)| *t != text) {
            let image = placeholder::render(&self.format, &text.0, &text.1);
            self.placeholder = Some((text, image));
        }
    }

    fn set_connected(&mut self, reading: bool) {
        if reading == self.connected {
            return;
        }
        self.connected = reading;
        if reading {
            // Start fresh: an old picture shouldn't flash up for the new reader.
            self.seq = None;
            self.fresh_at = None;
            self.frames.client_connected(self.format);
        } else {
            self.frames.client_disconnected();
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use rtspcam_ipc::{PixelFormat, StreamStatus};

    use super::*;

    const FORMAT: VideoFormat = VideoFormat {
        width: 16,
        height: 8,
        fps: 50,
        pixel_format: PixelFormat::Nv12,
    };

    struct FakeSource {
        /// The current picture and its sequence number.
        frame: Mutex<Option<(u64, Vec<u8>)>>,
        status: Mutex<(StreamStatus, String)>,
        connected: AtomicUsize,
        disconnected: AtomicUsize,
    }

    impl FakeSource {
        fn new(status: StreamStatus, message: &str) -> Arc<Self> {
            Arc::new(Self {
                frame: Mutex::new(None),
                status: Mutex::new((status, message.to_owned())),
                connected: AtomicUsize::new(0),
                disconnected: AtomicUsize::new(0),
            })
        }

        fn connected(&self) -> usize {
            self.connected.load(Ordering::SeqCst)
        }

        fn disconnected(&self) -> usize {
            self.disconnected.load(Ordering::SeqCst)
        }
    }

    impl FrameSource for FakeSource {
        fn client_connected(&self, format: VideoFormat) {
            assert_eq!(format, FORMAT);
            self.connected.fetch_add(1, Ordering::SeqCst);
        }

        fn client_disconnected(&self) {
            self.disconnected.fetch_add(1, Ordering::SeqCst);
        }

        fn next_frame(
            &self,
            _format: VideoFormat,
            after: Option<u64>,
            out: &mut Vec<u8>,
        ) -> Option<u64> {
            let frame = self.frame.lock().unwrap();
            let (seq, data) = frame.as_ref()?;
            if after == Some(*seq) {
                return None;
            }
            out.clear();
            out.extend_from_slice(data);
            Some(*seq)
        }

        fn status(&self) -> (StreamStatus, String) {
            self.status.lock().unwrap().clone()
        }
    }

    #[derive(Clone, Default)]
    struct FakeSink {
        writes: Arc<Mutex<Vec<Vec<u8>>>>,
        reader: Arc<Mutex<Option<bool>>>,
    }

    impl FakeSink {
        fn with_reader(reader: Option<bool>) -> Self {
            let sink = Self::default();
            *sink.reader.lock().unwrap() = reader;
            sink
        }

        fn writes(&self) -> Vec<Vec<u8>> {
            self.writes.lock().unwrap().clone()
        }

        fn count(&self) -> usize {
            self.writes.lock().unwrap().len()
        }

        fn set_reader(&self, reader: Option<bool>) {
            *self.reader.lock().unwrap() = reader;
        }
    }

    impl FrameSink for FakeSink {
        fn write(&mut self, frame: &[u8]) -> io::Result<()> {
            assert_eq!(frame.len(), FORMAT.frame_len());
            self.writes.lock().unwrap().push(frame.to_vec());
            Ok(())
        }

        fn has_reader(&mut self) -> Option<bool> {
            *self.reader.lock().unwrap()
        }
    }

    fn start(source: &Arc<FakeSource>, sink: &FakeSink, stale_after: Duration) -> Pusher {
        Pusher::start_with(
            "Door",
            FORMAT,
            source.clone(),
            Box::new(sink.clone()),
            stale_after,
        )
        .unwrap()
    }

    /// Waits up to two seconds for `cond`.
    fn wait_for(what: &str, cond: impl Fn() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(2);
        while !cond() {
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn picture(value: u8) -> Vec<u8> {
        vec![value; FORMAT.frame_len()]
    }

    #[test]
    fn writes_are_paced_to_the_frame_rate() {
        let source = FakeSource::new(StreamStatus::Connecting, "");
        let sink = FakeSink::default();
        let pusher = start(&source, &sink, STALE_AFTER);
        std::thread::sleep(Duration::from_millis(500));
        pusher.stop();
        // 50 fps for half a second: about 25. Never more (late wake-ups don't burst), but a busy
        // CI machine (macOS runners especially) wakes the thread late and skips ticks.
        let n = sink.count();
        assert!((5..=30).contains(&n), "{n} writes");
    }

    #[test]
    fn without_frames_the_status_placeholder_is_shown() {
        let source = FakeSource::new(StreamStatus::Connecting, "Opening stream");
        let sink = FakeSink::default();
        let pusher = start(&source, &sink, STALE_AFTER);
        wait_for("a write", || sink.count() > 0);
        let expected = placeholder::render(&FORMAT, "Connecting...", "Opening stream");
        assert_eq!(sink.writes()[0], expected);

        // A new status changes the picture.
        *source.status.lock().unwrap() = (StreamStatus::Error, "401 Unauthorized".into());
        let expected = placeholder::render(&FORMAT, "No signal", "401 Unauthorized");
        wait_for("the error placeholder", || {
            sink.writes().last() == Some(&expected)
        });
        pusher.stop();
    }

    #[test]
    fn the_last_frame_repeats_until_stale_then_the_placeholder() {
        let source = FakeSource::new(StreamStatus::Streaming, "");
        *source.frame.lock().unwrap() = Some((1, picture(77)));
        let sink = FakeSink::default();
        // Long enough for several ticks even when a busy CI machine wakes the thread late.
        let pusher = start(&source, &sink, Duration::from_millis(400));
        let no_signal = placeholder::render(&FORMAT, "No signal", "");
        wait_for("the placeholder", || sink.writes().contains(&no_signal));
        pusher.stop();

        let writes = sink.writes();
        let first_placeholder = writes.iter().position(|w| *w == no_signal).unwrap();
        // The one picture was written several times, then only the placeholder.
        assert!(first_placeholder >= 2, "{first_placeholder}");
        assert!(
            writes[..first_placeholder]
                .iter()
                .all(|w| *w == picture(77))
        );
        assert!(writes[first_placeholder..].iter().all(|w| *w == no_signal));
    }

    #[test]
    fn a_new_frame_replaces_the_placeholder() {
        let source = FakeSource::new(StreamStatus::Connecting, "");
        let sink = FakeSink::default();
        let pusher = start(&source, &sink, STALE_AFTER);
        wait_for("a write", || sink.count() > 0);
        *source.frame.lock().unwrap() = Some((1, picture(200)));
        wait_for("the frame", || sink.writes().last() == Some(&picture(200)));
        *source.frame.lock().unwrap() = Some((2, picture(201)));
        wait_for("the next frame", || {
            sink.writes().last() == Some(&picture(201))
        });
        pusher.stop();
    }

    #[test]
    fn readers_drive_the_client_count() {
        let source = FakeSource::new(StreamStatus::Disabled, "");
        let sink = FakeSink::with_reader(Some(false));
        let pusher = start(&source, &sink, STALE_AFTER);

        // Nobody reading: nothing fetched, written or reported.
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(sink.count(), 0);
        assert_eq!(source.connected(), 0);

        sink.set_reader(Some(true));
        wait_for("client_connected", || source.connected() == 1);
        wait_for("a write", || sink.count() > 0);

        sink.set_reader(Some(false));
        wait_for("client_disconnected", || source.disconnected() == 1);
        let written = sink.count();
        std::thread::sleep(Duration::from_millis(60));
        assert_eq!(sink.count(), written, "wrote with no reader");

        // A second reader later is a second connection.
        sink.set_reader(Some(true));
        wait_for("client_connected again", || source.connected() == 2);
        pusher.stop();
        assert_eq!(source.disconnected(), 2, "stopping ends the connection");
    }

    #[test]
    fn an_unknown_reader_counts_as_one_while_running() {
        let source = FakeSource::new(StreamStatus::Connecting, "");
        let sink = FakeSink::with_reader(None);
        let pusher = start(&source, &sink, STALE_AFTER);
        wait_for("client_connected", || source.connected() == 1);
        std::thread::sleep(Duration::from_millis(60));
        assert_eq!(source.connected(), 1);
        assert_eq!(source.disconnected(), 0);
        drop(pusher);
        assert_eq!(source.disconnected(), 1);
        assert!(sink.count() > 0);
    }

    #[test]
    fn write_errors_do_not_stop_delivery() {
        struct Flaky {
            calls: Arc<AtomicUsize>,
        }
        impl FrameSink for Flaky {
            fn write(&mut self, _frame: &[u8]) -> io::Result<()> {
                if self.calls.fetch_add(1, Ordering::SeqCst) < 3 {
                    Err(io::Error::other("device busy"))
                } else {
                    Ok(())
                }
            }
        }
        let calls = Arc::new(AtomicUsize::new(0));
        let source = FakeSource::new(StreamStatus::Connecting, "");
        let pusher = Pusher::start(
            "Door",
            FORMAT,
            source,
            Box::new(Flaky {
                calls: calls.clone(),
            }),
        )
        .unwrap();
        wait_for("writes after the errors", || {
            calls.load(Ordering::SeqCst) > 5
        });
        pusher.stop();
    }
}
