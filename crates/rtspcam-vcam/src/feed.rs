//! Receives frames from the app over the camera's pipe on a helper thread and keeps the newest.

use std::os::windows::io::AsRawHandle as _;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use rtspcam_ipc::client::{ERROR_FILE_NOT_FOUND, ERROR_PIPE_BUSY, FrameClient};
use rtspcam_ipc::{Message, StreamStatus, VideoFormat};
use uuid::Uuid;
use windows::Win32::Foundation::HANDLE;
use windows::Win32::System::IO::CancelSynchronousIo;

use crate::guard::ModuleRef;
use crate::log::log;

/// Why there is (or isn't) a picture.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum FeedStatus {
    /// The pipe doesn't exist: the app isn't running or doesn't know this camera.
    AppNotRunning,
    /// Connected to the app, nothing received yet.
    Waiting,
    /// The app's status for the stream.
    Stream(StreamStatus),
}

#[derive(Debug)]
pub(crate) struct Snapshot {
    pub frame: Option<Arc<Vec<u8>>>,
    /// Time since the frame arrived.
    pub age: Duration,
    pub status: FeedStatus,
    pub message: String,
}

#[derive(Debug)]
struct State {
    frame: Option<(Arc<Vec<u8>>, Instant)>,
    status: FeedStatus,
    message: String,
}

#[derive(Debug)]
struct Shared {
    state: Mutex<State>,
    stop: AtomicBool,
    /// Wakes the reconnect wait early on stop.
    wake: Condvar,
}

/// A running feed for one camera and format. Dropping it stops the helper thread.
#[derive(Debug)]
pub(crate) struct Feed {
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
}

impl Feed {
    pub(crate) fn start(camera: Uuid, format: VideoFormat) -> Self {
        let shared = Arc::new(Shared {
            state: Mutex::new(State {
                frame: None,
                status: FeedStatus::Waiting,
                message: String::new(),
            }),
            stop: AtomicBool::new(false),
            wake: Condvar::new(),
        });
        let thread_shared = shared.clone();
        let module = ModuleRef::new();
        let thread = std::thread::Builder::new()
            .name("rtspcam feed".into())
            .spawn(move || {
                let _module = module;
                run(camera, format, &thread_shared);
            })
            .map_err(|e| log!("could not start the feed thread: {e}"))
            .ok();
        Self { shared, thread }
    }

    pub(crate) fn snapshot(&self) -> Snapshot {
        let state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
        Snapshot {
            frame: state.frame.as_ref().map(|(f, _)| f.clone()),
            age: state
                .frame
                .as_ref()
                .map_or(Duration::MAX, |(_, at)| at.elapsed()),
            status: state.status.clone(),
            message: state.message.clone(),
        }
    }
}

impl Drop for Feed {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::SeqCst);
        self.shared.wake.notify_all();
        if let Some(thread) = self.thread.take() {
            // Break a blocking pipe read. If the thread isn't in a read right now it will see
            // the stop flag after its next message (the app sends one at least every second).
            // SAFETY: the handle stays valid while `thread` (the JoinHandle) is alive.
            let _ = unsafe { CancelSynchronousIo(HANDLE(thread.as_raw_handle())) };
            // Detach: never block a Media Foundation thread waiting for it.
        }
    }
}

fn set(shared: &Shared, f: impl FnOnce(&mut State)) {
    let mut state = shared.state.lock().unwrap_or_else(|e| e.into_inner());
    f(&mut state);
}

fn run(camera: Uuid, format: VideoFormat, shared: &Shared) {
    let mut last_error = None;
    while !shared.stop.load(Ordering::SeqCst) {
        let retry_in = match FrameClient::connect(camera, format) {
            Ok(mut client) => {
                log!("camera {camera}: connected to the app for {format}");
                last_error = None;
                set(shared, |s| s.status = FeedStatus::Waiting);
                receive(&mut client, format, shared);
                if shared.stop.load(Ordering::SeqCst) {
                    client.close();
                    break;
                }
                log!("camera {camera}: connection to the app ended");
                Duration::from_millis(500)
            }
            Err(e) => {
                let code = e.raw_os_error();
                if code == Some(ERROR_FILE_NOT_FOUND) {
                    set(shared, |s| {
                        s.status = FeedStatus::AppNotRunning;
                        s.frame = None;
                    });
                }
                if last_error != code {
                    log!("camera {camera}: can't open the app's pipe: {e}");
                    last_error = code;
                }
                if code == Some(ERROR_PIPE_BUSY) {
                    Duration::from_millis(50)
                } else {
                    Duration::from_secs(1)
                }
            }
        };
        let guard = shared.state.lock().unwrap_or_else(|e| e.into_inner());
        let _ = shared
            .wake
            .wait_timeout_while(guard, retry_in, |_| !shared.stop.load(Ordering::SeqCst));
    }
}

fn receive(client: &mut FrameClient, format: VideoFormat, shared: &Shared) {
    while !shared.stop.load(Ordering::SeqCst) {
        match client.read() {
            Ok(Message::Frame { header, data }) => {
                // Frames in another format can still be in the pipe after a format change.
                if header.format == format {
                    let frame = Arc::new(data.to_vec());
                    set(shared, |s| s.frame = Some((frame, Instant::now())));
                }
            }
            Ok(Message::Status { status, message }) => set(shared, |s| {
                s.status = FeedStatus::Stream(status);
                s.message = message;
            }),
            Ok(Message::Goodbye) => return,
            Ok(_) => {}
            Err(e) => {
                if !shared.stop.load(Ordering::SeqCst) {
                    log!("pipe read failed: {e}");
                }
                return;
            }
        }
    }
}
