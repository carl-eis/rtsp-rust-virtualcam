//! The media source's single video stream.
//!
//! `RequestSample` only records the request. A delivery thread answers requests at the
//! negotiated frame rate with the newest frame from the app (or a placeholder picture), so a
//! consumer that asks as fast as it can still gets a steady 30 (or 15) fps, and repeated frames
//! when the RTSP stream stalls.

use std::collections::VecDeque;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use rtspcam_ipc::{StreamStatus, VideoFormat};
use uuid::Uuid;
use windows::Win32::Media::KernelStreaming::PINNAME_VIDEO_CAPTURE;
use windows::Win32::Media::MediaFoundation::{
    IMFAsyncCallback, IMFAsyncResult, IMFAttributes, IMFMediaEvent, IMFMediaEventGenerator_Impl,
    IMFMediaEventQueue, IMFMediaSource, IMFMediaStream, IMFMediaStream_Impl, IMFMediaStream2,
    IMFMediaStream2_Impl, IMFStreamDescriptor, MEDIA_EVENT_GENERATOR_GET_EVENT_FLAGS,
    MEMediaSample, MEStreamStarted, MEStreamStopped, MF_DEVICESTREAM_ATTRIBUTE_FRAMESOURCE_TYPES,
    MF_DEVICESTREAM_FRAMESERVER_SHARED, MF_DEVICESTREAM_STREAM_CATEGORY, MF_DEVICESTREAM_STREAM_ID,
    MF_E_INVALIDREQUEST, MF_E_SHUTDOWN, MF_STREAM_STATE, MF_STREAM_STATE_RUNNING,
    MF_STREAM_STATE_STOPPED, MFCreateEventQueue, MFCreateMemoryBuffer, MFCreateSample,
    MFCreateStreamDescriptor, MFFrameSourceTypes_Color, MFGetSystemTime, MFSampleExtension_Token,
};
use windows::Win32::System::Com::StructuredStorage::PROPVARIANT;
use windows_core::{GUID, HRESULT, IUnknown, Interface as _, Ref, implement};

use crate::feed::{Feed, FeedStatus};
use crate::formats::{format_of, media_type};
use crate::guard::{Agile, ModuleRef, guard};
use crate::log::log;
use crate::placeholder;

/// A frame older than this counts as "no signal".
const STALE_AFTER: Duration = Duration::from_secs(3);

#[implement(IMFMediaStream2)]
pub(crate) struct MediaStream {
    inner: Mutex<Inner>,
    _module: ModuleRef,
}

struct Inner {
    /// `None` after shutdown.
    queue: Option<IMFMediaEventQueue>,
    /// Cleared on shutdown to break the source <-> stream reference cycle.
    source: Option<IMFMediaSource>,
    descriptor: IMFStreamDescriptor,
    state: MF_STREAM_STATE,
    camera: Option<Uuid>,
    delivery: Option<Delivery>,
}

impl MediaStream {
    /// Creates the stream and its descriptor offering `formats` (the first is the default).
    pub(crate) fn new(
        formats: &[VideoFormat],
    ) -> windows_core::Result<windows_core::ComObject<Self>> {
        // SAFETY: plain Media Foundation object creation and attribute setters.
        let descriptor = unsafe {
            let types = formats
                .iter()
                .map(|f| media_type(f).map(Some))
                .collect::<windows_core::Result<Vec<_>>>()?;
            let sd = MFCreateStreamDescriptor(0, &types)?;
            sd.GetMediaTypeHandler()?
                .SetCurrentMediaType(types[0].as_ref().expect("first media type"))?;
            sd.SetGUID(&MF_DEVICESTREAM_STREAM_CATEGORY, &PINNAME_VIDEO_CAPTURE)?;
            sd.SetUINT32(&MF_DEVICESTREAM_STREAM_ID, 0)?;
            sd.SetUINT32(&MF_DEVICESTREAM_FRAMESERVER_SHARED, 1)?;
            sd.SetUINT32(
                &MF_DEVICESTREAM_ATTRIBUTE_FRAMESOURCE_TYPES,
                MFFrameSourceTypes_Color.0 as u32,
            )?;
            sd
        };
        // SAFETY: creates a new event queue.
        let queue = unsafe { MFCreateEventQueue()? };
        Ok(windows_core::ComObject::new(Self {
            inner: Mutex::new(Inner {
                queue: Some(queue),
                source: None,
                descriptor,
                state: MF_STREAM_STATE_STOPPED,
                camera: None,
                delivery: None,
            }),
            _module: ModuleRef::new(),
        }))
    }

    pub(crate) fn set_source(&self, source: IMFMediaSource) {
        self.lock().source = Some(source);
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn queue(&self) -> windows_core::Result<IMFMediaEventQueue> {
        self.lock()
            .queue
            .clone()
            .ok_or_else(|| MF_E_SHUTDOWN.into())
    }

    pub(crate) fn descriptor(&self) -> IMFStreamDescriptor {
        self.lock().descriptor.clone()
    }

    /// Which app stream this camera shows. `None`: unknown (shows a placeholder).
    pub(crate) fn set_camera(&self, camera: Option<Uuid>) {
        self.lock().camera = camera;
    }

    /// Starts (or restarts) delivery in the format selected on `descriptor`.
    pub(crate) fn start(&self, descriptor: &IMFStreamDescriptor) -> windows_core::Result<()> {
        // SAFETY: valid descriptor from the presentation descriptor.
        let current = unsafe { descriptor.GetMediaTypeHandler()?.GetCurrentMediaType()? };
        let format = format_of(&current).ok_or(MF_E_INVALIDREQUEST)?;
        let mut inner = self.lock();
        let queue = inner.queue.clone().ok_or(MF_E_SHUTDOWN)?;
        inner.delivery = None; // stops a previous delivery thread
        inner.delivery = Some(Delivery::start(queue.clone(), format, inner.camera));
        inner.state = MF_STREAM_STATE_RUNNING;
        drop(inner);
        log!("stream started: {format}");
        queue_time_event(&queue, MEStreamStarted.0 as u32)
    }

    pub(crate) fn stop(&self) -> windows_core::Result<()> {
        let mut inner = self.lock();
        let was_running = inner.state == MF_STREAM_STATE_RUNNING;
        inner.delivery = None;
        inner.state = MF_STREAM_STATE_STOPPED;
        let queue = inner.queue.clone();
        drop(inner);
        if let (true, Some(queue)) = (was_running, queue) {
            log!("stream stopped");
            // SAFETY: valid queue; no payload.
            unsafe {
                queue.QueueEventParamVar(
                    MEStreamStopped.0 as u32,
                    &GUID::zeroed(),
                    HRESULT(0),
                    std::ptr::null(),
                )?;
            }
        }
        Ok(())
    }

    pub(crate) fn shutdown(&self) {
        let mut inner = self.lock();
        inner.delivery = None;
        inner.state = MF_STREAM_STATE_STOPPED;
        inner.source = None;
        if let Some(queue) = inner.queue.take() {
            // SAFETY: valid queue; shutting it down releases waiting callers.
            let _ = unsafe { queue.Shutdown() };
        }
    }
}

/// Queues an event whose value is the current Media Foundation system time.
pub(crate) fn queue_time_event(queue: &IMFMediaEventQueue, event: u32) -> windows_core::Result<()> {
    // SAFETY: MFGetSystemTime has no preconditions; the PROPVARIANT outlives the call.
    unsafe {
        let time = PROPVARIANT::from(MFGetSystemTime());
        queue.QueueEventParamVar(event, &GUID::zeroed(), HRESULT(0), &time)
    }
}

impl IMFMediaEventGenerator_Impl for MediaStream_Impl {
    fn GetEvent(
        &self,
        flags: MEDIA_EVENT_GENERATOR_GET_EVENT_FLAGS,
    ) -> windows_core::Result<IMFMediaEvent> {
        guard("Stream::GetEvent", || {
            let queue = self.queue()?;
            // SAFETY: delegates to a valid event queue, taken out of the lock first since
            // GetEvent may block.
            unsafe { queue.GetEvent(flags.0) }
        })
    }

    fn BeginGetEvent(
        &self,
        callback: Ref<IMFAsyncCallback>,
        state: Ref<IUnknown>,
    ) -> windows_core::Result<()> {
        guard("Stream::BeginGetEvent", || {
            let queue = self.queue()?;
            // SAFETY: delegates to a valid event queue, taken out of the lock first since
            // GetEvent may block.
            unsafe { queue.BeginGetEvent(callback.as_ref(), state.as_ref()) }
        })
    }

    fn EndGetEvent(&self, result: Ref<IMFAsyncResult>) -> windows_core::Result<IMFMediaEvent> {
        guard("Stream::EndGetEvent", || {
            let queue = self.queue()?;
            // SAFETY: delegates to a valid event queue, taken out of the lock first since
            // GetEvent may block.
            unsafe { queue.EndGetEvent(result.as_ref()) }
        })
    }

    fn QueueEvent(
        &self,
        met: u32,
        extended: *const GUID,
        status: HRESULT,
        value: *const PROPVARIANT,
    ) -> windows_core::Result<()> {
        guard("Stream::QueueEvent", || {
            let queue = self.queue()?;
            // SAFETY: delegates to a valid event queue, taken out of the lock first since
            // GetEvent may block.
            unsafe { queue.QueueEventParamVar(met, extended, status, value) }
        })
    }
}

impl IMFMediaStream_Impl for MediaStream_Impl {
    fn GetMediaSource(&self) -> windows_core::Result<IMFMediaSource> {
        guard("Stream::GetMediaSource", || {
            self.lock()
                .source
                .clone()
                .ok_or_else(|| MF_E_SHUTDOWN.into())
        })
    }

    fn GetStreamDescriptor(&self) -> windows_core::Result<IMFStreamDescriptor> {
        guard("Stream::GetStreamDescriptor", || {
            let inner = self.lock();
            inner.queue.as_ref().ok_or(MF_E_SHUTDOWN)?;
            Ok(inner.descriptor.clone())
        })
    }

    fn RequestSample(&self, token: Ref<IUnknown>) -> windows_core::Result<()> {
        guard("Stream::RequestSample", || {
            let inner = self.lock();
            inner.queue.as_ref().ok_or(MF_E_SHUTDOWN)?;
            match (&inner.delivery, inner.state == MF_STREAM_STATE_RUNNING) {
                (Some(d), true) => {
                    d.request(token.as_ref().cloned());
                    Ok(())
                }
                _ => Err(MF_E_INVALIDREQUEST.into()),
            }
        })
    }
}

impl IMFMediaStream2_Impl for MediaStream_Impl {
    fn SetStreamState(&self, value: MF_STREAM_STATE) -> windows_core::Result<()> {
        guard("Stream::SetStreamState", || {
            log!("SetStreamState({})", value.0);
            if value == MF_STREAM_STATE_STOPPED {
                self.stop()
            } else if value == MF_STREAM_STATE_RUNNING
                && self.lock().state != MF_STREAM_STATE_RUNNING
            {
                let descriptor = self.descriptor();
                self.start(&descriptor)
            } else {
                Ok(())
            }
        })
    }

    fn GetStreamState(&self) -> windows_core::Result<MF_STREAM_STATE> {
        guard("Stream::GetStreamState", || Ok(self.lock().state))
    }
}

/// Requests waiting for a sample, shared with the delivery thread.
struct Requests {
    tokens: VecDeque<Option<Agile<IUnknown>>>,
    running: bool,
}

/// The delivery thread for one start of the stream. Dropping it stops the thread.
struct Delivery {
    shared: Arc<(Mutex<Requests>, Condvar)>,
}

impl Delivery {
    fn start(queue: IMFMediaEventQueue, format: VideoFormat, camera: Option<Uuid>) -> Self {
        let shared = Arc::new((
            Mutex::new(Requests {
                tokens: VecDeque::new(),
                running: true,
            }),
            Condvar::new(),
        ));
        let thread_shared = shared.clone();
        let queue = Agile(queue);
        let module = ModuleRef::new();
        let spawned = std::thread::Builder::new()
            .name("rtspcam delivery".into())
            .spawn(move || {
                let _module = module;
                let queue = queue;
                deliver(&queue.0, format, camera, &thread_shared);
            });
        if let Err(e) = spawned {
            log!("could not start the delivery thread: {e}");
        }
        Self { shared }
    }

    fn request(&self, token: Option<IUnknown>) {
        let (lock, cv) = &*self.shared;
        let mut r = lock.lock().unwrap_or_else(|e| e.into_inner());
        r.tokens.push_back(token.map(Agile));
        cv.notify_one();
    }
}

impl Drop for Delivery {
    fn drop(&mut self) {
        let (lock, cv) = &*self.shared;
        lock.lock().unwrap_or_else(|e| e.into_inner()).running = false;
        cv.notify_one();
    }
}

fn deliver(
    queue: &IMFMediaEventQueue,
    format: VideoFormat,
    camera: Option<Uuid>,
    shared: &(Mutex<Requests>, Condvar),
) {
    let (lock, cv) = shared;
    let interval = Duration::from_secs(1) / format.fps;
    let feed = camera.map(|id| Feed::start(id, format));
    let mut placeholder: Option<((String, String), Vec<u8>)> = None;
    let mut next_due = Instant::now();
    loop {
        // Wait for a request.
        let token = {
            let mut r = lock.lock().unwrap_or_else(|e| e.into_inner());
            while r.running && r.tokens.is_empty() {
                r = cv.wait(r).unwrap_or_else(|e| e.into_inner());
            }
            if !r.running {
                return;
            }
            r.tokens.pop_front().flatten()
        };
        // Pace to the frame rate; a long gap between requests doesn't cause a burst.
        let now = Instant::now();
        if next_due > now {
            let r = lock.lock().unwrap_or_else(|e| e.into_inner());
            let (r, _) = cv
                .wait_timeout_while(r, next_due - now, |r| r.running)
                .unwrap_or_else(|e| e.into_inner());
            if !r.running {
                return;
            }
        }
        next_due = next_due.max(Instant::now() - interval) + interval;

        let snapshot = feed.as_ref().map(Feed::snapshot);
        let live = snapshot
            .as_ref()
            .and_then(|s| s.frame.clone().filter(|_| s.age < STALE_AFTER));
        let data: &[u8] = match &live {
            Some(frame) => frame,
            None => {
                let text = placeholder_text(
                    camera,
                    snapshot.as_ref().map(|s| (&s.status, s.message.as_str())),
                );
                if placeholder.as_ref().is_none_or(|(t, _)| *t != text) {
                    let image = placeholder::render(&format, &text.0, &text.1);
                    placeholder = Some((text, image));
                }
                &placeholder
                    .as_ref()
                    .expect("placeholder was just rendered")
                    .1
            }
        };
        if let Err(e) = send_sample(queue, data, interval, token.as_ref().map(|t| &t.0)) {
            log!("could not deliver a sample: {e}");
            if e.code() == MF_E_SHUTDOWN {
                return;
            }
        }
    }
}

fn placeholder_text(camera: Option<Uuid>, status: Option<(&FeedStatus, &str)>) -> (String, String) {
    let (title, detail) = match (camera, status) {
        (None, _) | (_, None) => ("RTSP Cam", "This camera is not set up in the RTSP Cam app"),
        (_, Some((FeedStatus::AppNotRunning, _))) => (
            "RTSP Cam is not running",
            "Start RTSP Cam to use this camera",
        ),
        (_, Some((FeedStatus::Waiting, _))) => ("Connecting...", ""),
        (_, Some((FeedStatus::Stream(StreamStatus::Connecting), m))) => ("Connecting...", m),
        (_, Some((FeedStatus::Stream(StreamStatus::Disabled), _))) => ("Camera disabled", ""),
        (_, Some((FeedStatus::Stream(StreamStatus::Error), m))) => ("No signal", m),
        (_, Some((FeedStatus::Stream(StreamStatus::Streaming), _))) => ("No signal", ""),
    };
    (title.to_owned(), detail.to_owned())
}

fn send_sample(
    queue: &IMFMediaEventQueue,
    data: &[u8],
    duration: Duration,
    token: Option<&IUnknown>,
) -> windows_core::Result<()> {
    let len = u32::try_from(data.len()).map_err(|_| MF_E_INVALIDREQUEST)?;
    // SAFETY: a fresh buffer of `len` bytes; the locked pointer is valid for `len` bytes until
    // Unlock. The sample and event queue are valid COM objects.
    unsafe {
        let buffer = MFCreateMemoryBuffer(len)?;
        let mut ptr = std::ptr::null_mut();
        buffer.Lock(&mut ptr, None, None)?;
        std::ptr::copy_nonoverlapping(data.as_ptr(), ptr, data.len());
        buffer.Unlock()?;
        buffer.SetCurrentLength(len)?;
        let sample = MFCreateSample()?;
        sample.AddBuffer(&buffer)?;
        sample.SetSampleTime(MFGetSystemTime())?;
        sample.SetSampleDuration((duration.as_nanos() / 100) as i64)?;
        if let Some(token) = token {
            sample.SetUnknown(&MFSampleExtension_Token, token)?;
        }
        queue.QueueEventParamUnk(MEMediaSample.0 as u32, &GUID::zeroed(), HRESULT(0), &sample)
    }
}

/// The stream's attributes (the descriptor carries the `MF_DEVICESTREAM_*` values).
pub(crate) fn stream_attributes(stream: &MediaStream) -> windows_core::Result<IMFAttributes> {
    stream.descriptor().cast()
}

/// The stream as the interface the source hands out.
pub(crate) fn as_interface(stream: &windows_core::ComObject<MediaStream>) -> IMFMediaStream {
    stream.to_interface::<IMFMediaStream2>().into()
}
