//! The media source's single video stream.
//!
//! `RequestSample` only records the request. A delivery thread answers requests at the
//! negotiated frame rate with the newest frame from the app (or a placeholder picture), so a
//! consumer that asks as fast as it can still gets a steady 30 (or 15) fps, and repeated frames
//! when the RTSP stream stalls.
//!
//! The thread waits for each frame slot on a high-resolution waitable timer: a condition
//! variable's timeout is rounded to the process's timer resolution (about 15.6 ms in Frame
//! Server), which made frame gaps alternate between about 31 and 47 ms at 30 fps. Samples are
//! stamped `start + n * interval`, so wake-up jitter doesn't reach the timestamps either.

use std::collections::VecDeque;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use rtspcam_ipc::VideoFormat;
use rtspcam_ipc::placeholder::{self, STALE_AFTER};
use uuid::Uuid;
use windows::Win32::Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0};
use windows::Win32::Media::KernelStreaming::PINNAME_VIDEO_CAPTURE;
use windows::Win32::Media::MediaFoundation::{
    IMF2DBuffer, IMFAsyncCallback, IMFAsyncResult, IMFAttributes, IMFMediaEvent,
    IMFMediaEventGenerator_Impl, IMFMediaEventQueue, IMFMediaSource, IMFMediaStream,
    IMFMediaStream_Impl, IMFMediaStream2, IMFMediaStream2_Impl, IMFSample, IMFStreamDescriptor,
    IMFVideoSampleAllocatorEx, MEDIA_EVENT_GENERATOR_GET_EVENT_FLAGS, MEError, MEMediaSample,
    MEStreamStarted, MEStreamStopped, MF_DEVICESTREAM_ATTRIBUTE_FRAMESOURCE_TYPES,
    MF_DEVICESTREAM_FRAMESERVER_SHARED, MF_DEVICESTREAM_STREAM_CATEGORY, MF_DEVICESTREAM_STREAM_ID,
    MF_E_INVALIDREQUEST, MF_E_SHUTDOWN, MF_STREAM_STATE, MF_STREAM_STATE_RUNNING,
    MF_STREAM_STATE_STOPPED, MFCreateEventQueue, MFCreateMemoryBuffer, MFCreateSample,
    MFCreateStreamDescriptor, MFCreateVideoSampleAllocatorEx, MFFrameSourceTypes_Color,
    MFGetSystemTime, MFSampleExtension_Token,
};
use windows::Win32::System::Com::StructuredStorage::PROPVARIANT;
use windows::Win32::System::Threading::{
    CREATE_WAITABLE_TIMER_HIGH_RESOLUTION, CreateEventW, CreateWaitableTimerExW, INFINITE,
    SetEvent, SetWaitableTimer, TIMER_ALL_ACCESS, WaitForMultipleObjects,
};
use windows_core::{GUID, HRESULT, IUnknown, Interface as _, PCWSTR, Ref, implement};

use crate::feed::{Feed, FeedStatus};
use crate::formats::{format_of, media_type};
use crate::guard::{Agile, ModuleRef, guard};
use crate::log::log;

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

/// Requests waiting for a sample.
struct Requests {
    tokens: VecDeque<Option<Agile<IUnknown>>>,
    running: bool,
}

/// What the stream shares with its delivery thread.
struct Shared {
    requests: Mutex<Requests>,
    /// Signalled when a request arrives or delivery stops.
    arrived: Condvar,
    /// Set (once, never reset) when delivery stops, to end a wait for the next frame slot.
    /// `None` if it couldn't be created; the thread then paces with `arrived`.
    stopped: Option<Handle>,
}

impl Shared {
    fn lock(&self) -> std::sync::MutexGuard<'_, Requests> {
        self.requests.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// An owned kernel handle (event or timer), closed on drop.
struct Handle(HANDLE);

// SAFETY: event and timer handles may be used and closed from any thread.
unsafe impl Send for Handle {}
// SAFETY: as above; waiting and signalling are thread-safe kernel calls.
unsafe impl Sync for Handle {}

impl Drop for Handle {
    fn drop(&mut self) {
        // SAFETY: the handle is owned by this value and not used after this.
        let _ = unsafe { CloseHandle(self.0) };
    }
}

/// A manual-reset event, initially not set.
fn new_event() -> Option<Handle> {
    // SAFETY: creates an unnamed event; the returned handle is owned by `Handle`.
    unsafe { CreateEventW(None, true, false, PCWSTR::null()) }
        .map(Handle)
        .map_err(|e| log!("could not create the stop event: {e}"))
        .ok()
}

/// A high-resolution waitable timer (Windows 10 1803 and later), or a normal one before that.
fn new_timer() -> Option<Handle> {
    let create = |flags| {
        // SAFETY: creates an unnamed timer; the returned handle is owned by `Handle`.
        unsafe { CreateWaitableTimerExW(None, PCWSTR::null(), flags, TIMER_ALL_ACCESS.0) }
    };
    create(CREATE_WAITABLE_TIMER_HIGH_RESOLUTION)
        .or_else(|e| {
            log!("no high-resolution timer ({e}); frame pacing will be coarser");
            create(0)
        })
        .map(Handle)
        .map_err(|e| log!("could not create a waitable timer: {e}"))
        .ok()
}

/// The delivery thread for one start of the stream. Dropping it stops the thread.
struct Delivery {
    shared: Arc<Shared>,
}

impl Delivery {
    fn start(queue: IMFMediaEventQueue, format: VideoFormat, camera: Option<Uuid>) -> Self {
        let shared = Arc::new(Shared {
            requests: Mutex::new(Requests {
                tokens: VecDeque::new(),
                running: true,
            }),
            arrived: Condvar::new(),
            stopped: new_event(),
        });
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
        self.shared.lock().tokens.push_back(token.map(Agile));
        self.shared.arrived.notify_one();
    }
}

impl Drop for Delivery {
    fn drop(&mut self) {
        self.shared.lock().running = false;
        self.shared.arrived.notify_one();
        if let Some(stopped) = &self.shared.stopped {
            // SAFETY: a valid event handle, kept alive by `shared`.
            let _ = unsafe { SetEvent(stopped.0) };
        }
    }
}

/// Waits until `deadline` or until delivery stops. Returns whether delivery is still running.
fn wait_until(shared: &Shared, timer: Option<&Handle>, deadline: Instant) -> bool {
    let remaining = deadline.saturating_duration_since(Instant::now());
    if !remaining.is_zero() {
        let waited = match (timer, &shared.stopped) {
            (Some(timer), Some(stopped)) => {
                // Relative due time: negative, in 100 ns units.
                let due = -i64::try_from(remaining.as_nanos() / 100)
                    .unwrap_or(i64::MAX)
                    .max(1);
                // SAFETY: valid timer and event handles, owned by the caller and `shared` for
                // the whole call; no completion routine.
                unsafe {
                    SetWaitableTimer(timer.0, &due, 0, None, None, false).is_ok() && {
                        let woke = WaitForMultipleObjects(&[timer.0, stopped.0], false, INFINITE);
                        // WAIT_OBJECT_0 + 1 is the stop event; anything else is a failure.
                        woke.0 <= WAIT_OBJECT_0.0 + 1
                    }
                }
            }
            _ => false,
        };
        if !waited {
            // No timer: the condvar's timeout, rounded to the timer resolution.
            let r = shared.lock();
            let _ = shared
                .arrived
                .wait_timeout_while(r, remaining, |r| r.running)
                .unwrap_or_else(|e| e.into_inner());
        }
    }
    shared.lock().running
}

/// Evenly spaced sample times: `start + n * interval` in 100 ns units, re-anchored to the clock
/// only when it has drifted by more than [`Timeline::MAX_DRIFT`] intervals (after a long gap
/// between requests, or a consumer that asks slower than the frame rate).
struct Timeline {
    fps: i64,
    start: i64,
    n: i64,
    anchored: bool,
}

impl Timeline {
    const MAX_DRIFT: i64 = 2;

    fn new(fps: u32) -> Self {
        Self {
            fps: i64::from(fps.max(1)),
            start: 0,
            n: 0,
            anchored: false,
        }
    }

    /// The time for the next sample, given the clock `now` (100 ns units).
    fn next(&mut self, now: i64) -> i64 {
        let interval = 10_000_000 / self.fps;
        let mut time = self.at(self.n);
        if !self.anchored || (now - time).abs() > Self::MAX_DRIFT * interval {
            self.start = now;
            self.n = 0;
            self.anchored = true;
            time = now;
        }
        self.n += 1;
        time
    }

    fn at(&self, n: i64) -> i64 {
        // Multiply before dividing, so 30 fps doesn't drift by a third of a tick per frame.
        self.start + n * 10_000_000 / self.fps
    }
}

/// How many samples the pool starts with and may grow to. Frame Server holds a few at a time;
/// past the maximum, samples are allocated one-off (see [`send_sample`]).
const POOL_INITIAL: u32 = 2;
const POOL_MAX: u32 = 8;

/// A pool of system-memory samples for `format`, recycled when Frame Server releases them, the
/// way Microsoft's virtual camera sample does it. `None` if Media Foundation won't make one.
fn sample_pool(format: &VideoFormat) -> Option<IMFVideoSampleAllocatorEx> {
    let pool = || -> windows_core::Result<IMFVideoSampleAllocatorEx> {
        let media_type = media_type(format)?;
        let mut raw = std::ptr::null_mut();
        // SAFETY: on success `raw` is an owned IMFVideoSampleAllocatorEx pointer, taken over by
        // `from_raw`. No device manager is set, so samples are in system memory.
        unsafe {
            MFCreateVideoSampleAllocatorEx(&IMFVideoSampleAllocatorEx::IID, &mut raw)?;
            let pool = IMFVideoSampleAllocatorEx::from_raw(raw);
            pool.InitializeSampleAllocatorEx(POOL_INITIAL, POOL_MAX, None, &media_type)?;
            Ok(pool)
        }
    };
    pool()
        .map_err(|e| log!("no sample pool for {format} ({e}); allocating per frame"))
        .ok()
}

fn deliver(queue: &IMFMediaEventQueue, format: VideoFormat, camera: Option<Uuid>, shared: &Shared) {
    let interval = Duration::from_secs(1) / format.fps;
    let feed = camera.map(|id| Feed::start(id, format));
    let timer = new_timer();
    let pool = sample_pool(&format);
    let mut timeline = Timeline::new(format.fps);
    let mut placeholder: Option<((String, String), Vec<u8>)> = None;
    let mut next_due = Instant::now();
    loop {
        // Wait for a request.
        let token = {
            let mut r = shared.lock();
            while r.running && r.tokens.is_empty() {
                r = shared.arrived.wait(r).unwrap_or_else(|e| e.into_inner());
            }
            if !r.running {
                return;
            }
            r.tokens.pop_front().flatten()
        };
        // Pace to the frame rate; a long gap between requests doesn't cause a burst.
        if !wait_until(shared, timer.as_ref(), next_due) {
            return;
        }
        next_due = next_due.max(Instant::now() - interval) + interval;
        // SAFETY: MFGetSystemTime has no preconditions.
        let time = timeline.next(unsafe { MFGetSystemTime() });

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
        let token = token.as_ref().map(|t| &t.0);
        let sent = send_or_report(queue, |pool_allowed| {
            let pool = pool.as_ref().filter(|_| pool_allowed);
            send_sample(queue, pool, data, time, interval, token)
        });
        if !sent {
            return;
        }
    }
}

/// Answers one request with `send(pool_allowed)`. If that fails, retries once without the pool
/// (a one-off sample), and if that fails too, queues `MEError` so Frame Server closes and
/// reopens the stream: a request without a sample is lost for good, and Frame Server keeps only
/// a few open, so after a few of those the stream would wait forever. Returns `false` once the
/// stream has shut down.
fn send_or_report(
    queue: &IMFMediaEventQueue,
    mut send: impl FnMut(bool) -> windows_core::Result<()>,
) -> bool {
    let error = match send(true).or_else(|e| {
        if e.code() == MF_E_SHUTDOWN {
            return Err(e);
        }
        log!("could not deliver a sample ({e}); trying a new one");
        send(false)
    }) {
        Ok(()) => return true,
        Err(e) if e.code() == MF_E_SHUTDOWN => return false,
        Err(e) => e,
    };
    log!("could not deliver a sample: {error}; reporting the error");
    // SAFETY: valid queue; no payload.
    let queued = unsafe {
        queue.QueueEventParamVar(
            MEError.0 as u32,
            &GUID::zeroed(),
            error.code(),
            std::ptr::null(),
        )
    };
    match queued {
        Ok(()) => true,
        Err(e) => {
            log!("could not report the error: {e}");
            e.code() != MF_E_SHUTDOWN
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
        (_, Some((FeedStatus::Stream(status), m))) => placeholder::text_for_status(*status, m),
    };
    (title.to_owned(), detail.to_owned())
}

/// Sends `data` as a sample stamped `time` (100 ns units), in a pooled sample when the pool has
/// one free and a one-off sample otherwise.
fn send_sample(
    queue: &IMFMediaEventQueue,
    pool: Option<&IMFVideoSampleAllocatorEx>,
    data: &[u8],
    time: i64,
    duration: Duration,
    token: Option<&IUnknown>,
) -> windows_core::Result<()> {
    let sample = match pool.and_then(|pool| pooled_sample(pool, data)) {
        Some(sample) => sample,
        None => one_off_sample(data)?,
    };
    // SAFETY: the sample and event queue are valid COM objects.
    unsafe {
        sample.SetSampleTime(time)?;
        sample.SetSampleDuration((duration.as_nanos() / 100) as i64)?;
        if let Some(token) = token {
            sample.SetUnknown(&MFSampleExtension_Token, token)?;
        }
        queue.QueueEventParamUnk(MEMediaSample.0 as u32, &GUID::zeroed(), HRESULT(0), &sample)
    }
}

/// A sample from `pool` holding a copy of `data`, or `None` if the pool is empty (Frame Server
/// still holds every pooled sample) or its buffers don't fit `data`.
fn pooled_sample(pool: &IMFVideoSampleAllocatorEx, data: &[u8]) -> Option<IMFSample> {
    let len = u32::try_from(data.len()).ok()?;
    // SAFETY: valid COM objects. `ContiguousCopyFrom` copies exactly `data` (checked against
    // the buffer's contiguous length first) and handles the buffer's row pitch.
    unsafe {
        let sample = pool.AllocateSample().ok()?;
        // A recycled sample keeps the attributes of its last trip (the previous request's
        // token, anything Frame Server added).
        sample.DeleteAllItems().ok()?;
        let buffer = sample.GetBufferByIndex(0).ok()?;
        let buffer_2d = buffer.cast::<IMF2DBuffer>().ok()?;
        if buffer_2d.GetContiguousLength().ok()? != len {
            return None;
        }
        buffer_2d.ContiguousCopyFrom(data).ok()?;
        buffer.SetCurrentLength(len).ok()?;
        Some(sample)
    }
}

/// A new sample with a new buffer holding a copy of `data`.
fn one_off_sample(data: &[u8]) -> windows_core::Result<IMFSample> {
    let len = u32::try_from(data.len()).map_err(|_| MF_E_INVALIDREQUEST)?;
    // SAFETY: a fresh buffer of `len` bytes; the locked pointer is valid for `len` bytes until
    // Unlock.
    unsafe {
        let buffer = MFCreateMemoryBuffer(len)?;
        let mut ptr = std::ptr::null_mut();
        buffer.Lock(&mut ptr, None, None)?;
        std::ptr::copy_nonoverlapping(data.as_ptr(), ptr, data.len());
        buffer.Unlock()?;
        buffer.SetCurrentLength(len)?;
        let sample = MFCreateSample()?;
        sample.AddBuffer(&buffer)?;
        Ok(sample)
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

#[cfg(test)]
mod tests {
    use windows::Win32::Foundation::E_OUTOFMEMORY;

    use super::*;

    #[test]
    fn timeline_steps_by_exactly_one_interval() {
        let mut t = Timeline::new(30);
        let start = 1_000_000_000;
        // Wake-up jitter of a few ms doesn't reach the timestamps.
        let jitter = [0, 120_000, -80_000, 140_000, 0, -50_000];
        let times: Vec<i64> = (0..6)
            .map(|n| t.next(start + n * 333_333 + jitter[n as usize]))
            .collect();
        assert_eq!(times[0], start);
        for (n, time) in times.iter().enumerate() {
            assert_eq!(*time, start + n as i64 * 10_000_000 / 30);
        }
        // No drift from rounding 1/30 s: after 30 frames, exactly one second.
        for _ in 6..30 {
            let n = t.n;
            t.next(start + n * 333_333);
        }
        assert_eq!(t.next(start + 10_000_000), start + 10_000_000);
    }

    #[test]
    fn timeline_reanchors_after_a_long_gap() {
        let mut t = Timeline::new(30);
        assert_eq!(t.next(0), 0);
        assert_eq!(t.next(333_333), 333_333);
        // Within two intervals of the slot: stays on the grid.
        assert_eq!(t.next(666_666 + 600_000), 666_666);
        // A second without requests: follows the clock again.
        assert_eq!(t.next(20_000_000), 20_000_000);
        assert_eq!(t.next(20_333_333), 20_333_333);
    }

    /// The next event on `queue`, if one is waiting.
    fn next_event(queue: &IMFMediaEventQueue) -> Option<(u32, HRESULT)> {
        use windows::Win32::Media::MediaFoundation::MF_EVENT_FLAG_NO_WAIT;
        // SAFETY: a valid queue; NO_WAIT returns at once.
        unsafe {
            let event = queue.GetEvent(MF_EVENT_FLAG_NO_WAIT.0).ok()?;
            Some((event.GetType().ok()?, event.GetStatus().ok()?))
        }
    }

    fn event_queue() -> IMFMediaEventQueue {
        // SAFETY: creates a new event queue.
        unsafe { MFCreateEventQueue().unwrap() }
    }

    #[test]
    fn a_failed_sample_is_retried_without_the_pool() {
        let queue = event_queue();
        let mut tries = Vec::new();
        let sent = send_or_report(&queue, |pool| {
            tries.push(pool);
            if pool {
                Err(E_OUTOFMEMORY.into())
            } else {
                Ok(())
            }
        });
        assert!(sent);
        assert_eq!(tries, [true, false]);
        assert_eq!(next_event(&queue), None, "no error reported");
    }

    #[test]
    fn a_sample_that_cannot_be_sent_queues_me_error() {
        let queue = event_queue();
        let sent = send_or_report(&queue, |_| Err(E_OUTOFMEMORY.into()));
        assert!(sent, "delivery goes on");
        assert_eq!(next_event(&queue), Some((MEError.0 as u32, E_OUTOFMEMORY)));
    }

    #[test]
    fn shutdown_ends_delivery_without_a_retry() {
        let queue = event_queue();
        let mut tries = 0;
        let sent = send_or_report(&queue, |_| {
            tries += 1;
            Err(MF_E_SHUTDOWN.into())
        });
        assert!(!sent);
        assert_eq!(tries, 1);
        assert_eq!(next_event(&queue), None);
    }
}
