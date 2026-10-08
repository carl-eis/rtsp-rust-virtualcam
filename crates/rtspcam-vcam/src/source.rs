//! The custom media source: one live video stream.
//!
//! State machine: `Start` (re)starts the selected stream and queues `MENewStream` (first time)
//! or `MEUpdatedStream`, then `MESourceStarted`; `Stop` queues `MESourceStopped`; `Pause` is
//! not supported (live source); `Shutdown` releases everything and every later call fails with
//! `MF_E_SHUTDOWN`.

use std::sync::Mutex;

use uuid::Uuid;
use windows::Win32::Foundation::{E_POINTER, ERROR_SET_NOT_FOUND};
use windows::Win32::Media::KernelStreaming::{IKsControl, IKsControl_Impl, KSIDENTIFIER};
use windows::Win32::Media::MediaFoundation::{
    IMFAsyncCallback, IMFAsyncResult, IMFAttributes, IMFGetService, IMFGetService_Impl,
    IMFMediaEvent, IMFMediaEventGenerator_Impl, IMFMediaEventQueue, IMFMediaSource,
    IMFMediaSource_Impl, IMFMediaSourceEx, IMFMediaSourceEx_Impl, IMFPresentationDescriptor,
    IMFStreamDescriptor, MEDIA_EVENT_GENERATOR_GET_EVENT_FLAGS, MENewStream, MESourceStarted,
    MESourceStopped, MEUpdatedStream, MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_SYMBOLIC_LINK,
    MF_E_INVALID_STATE_TRANSITION, MF_E_INVALIDSTREAMNUMBER, MF_E_SHUTDOWN,
    MF_E_UNSUPPORTED_SERVICE, MF_E_UNSUPPORTED_TIME_FORMAT, MFCreateAttributes, MFCreateEventQueue,
    MFCreatePresentationDescriptor, MFMEDIASOURCE_IS_LIVE,
};
use windows::Win32::System::Com::StructuredStorage::PROPVARIANT;
use windows_core::{BOOL, ComObject, GUID, HRESULT, IUnknown, Interface as _, Ref, implement};

use crate::camera_id::{self, CameraInfo};
use crate::formats::advertised;
use crate::guard::{ModuleRef, guard};
use crate::log::log;
use crate::stream::{self, MediaStream, queue_time_event};

/// Our own attribute (string): the camera id, for in-process tests and tools that create the
/// source without a virtual camera device. Frame Server uses the device property instead.
pub const RTSPCAM_ATTR_CAMERA_ID: GUID = GUID::from_u128(0xc5464fce_84dc_4420_8bc5_53830a558a46);

#[implement(IMFMediaSourceEx, IMFGetService, IKsControl)]
pub(crate) struct MediaSource {
    inner: Mutex<Inner>,
    /// The activation object's attributes, where Frame Server puts the device's symbolic link.
    activate_attributes: IMFAttributes,
    _module: ModuleRef,
}

struct Inner {
    /// `None` after shutdown.
    queue: Option<IMFMediaEventQueue>,
    attributes: IMFAttributes,
    descriptor: IMFPresentationDescriptor,
    stream: Option<ComObject<MediaStream>>,
    started_once: bool,
    camera: Option<CameraInfo>,
}

impl MediaSource {
    pub(crate) fn create(
        activate_attributes: IMFAttributes,
    ) -> windows_core::Result<IMFMediaSource> {
        let camera = resolve_camera(&activate_attributes, None);
        // SAFETY: plain Media Foundation object creation.
        let (queue, attributes) = unsafe {
            let mut attributes = None;
            MFCreateAttributes(&mut attributes, 4)?;
            (MFCreateEventQueue()?, attributes.ok_or(E_POINTER)?)
        };
        let preferred = camera.as_ref().and_then(|c| c.preferred);
        let stream = MediaStream::new(&advertised(preferred))?;
        stream.set_camera(camera.as_ref().map(|c| c.id));
        let sd: IMFStreamDescriptor = stream.descriptor();
        // SAFETY: one stream descriptor, selected by default.
        let descriptor = unsafe {
            let pd = MFCreatePresentationDescriptor(Some(&[Some(sd)]))?;
            pd.SelectStream(0)?;
            pd
        };
        let source = ComObject::new(Self {
            inner: Mutex::new(Inner {
                queue: Some(queue),
                attributes,
                descriptor,
                stream: Some(stream.clone()),
                started_once: false,
                camera: camera.clone(),
            }),
            activate_attributes,
            _module: ModuleRef::new(),
        });
        let interface: IMFMediaSource = source.to_interface::<IMFMediaSourceEx>().into();
        // The stream refers back to its source (cleared on shutdown to break the cycle).
        stream.set_source(interface.clone());
        log!(
            "media source created (camera {})",
            camera.map_or_else(|| "unknown".to_owned(), |c| c.id.to_string())
        );
        Ok(interface)
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
}

/// Works out which app stream this camera is, from (in order) our own attribute, then the
/// device's symbolic link on the activation object or the source's attributes.
fn resolve_camera(activate: &IMFAttributes, source: Option<&IMFAttributes>) -> Option<CameraInfo> {
    if let Some(id) =
        get_string(activate, &RTSPCAM_ATTR_CAMERA_ID).and_then(|s| Uuid::parse_str(&s).ok())
    {
        return Some(CameraInfo {
            id,
            preferred: None,
        });
    }
    for attrs in std::iter::once(activate).chain(source) {
        if let Some(link) = get_string(
            attrs,
            &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_SYMBOLIC_LINK,
        ) {
            match camera_id::lookup(&link) {
                Some(info) => return Some(info),
                None => log!("no camera id on device {link}"),
            }
        }
    }
    None
}

fn get_string(attrs: &IMFAttributes, key: &GUID) -> Option<String> {
    // SAFETY: GetStringLength/GetString with a buffer of the reported length plus terminator.
    unsafe {
        let len = attrs.GetStringLength(key).ok()? as usize;
        let mut buf = vec![0u16; len + 1];
        let mut written = 0;
        attrs.GetString(key, &mut buf, Some(&mut written)).ok()?;
        Some(String::from_utf16_lossy(&buf[..written as usize]))
    }
}

/// Logs every attribute key, to see what Frame Server provides (spike S0.2).
pub(crate) fn log_attribute_keys(what: &str, attrs: &IMFAttributes) {
    // SAFETY: index-based enumeration within GetCount.
    unsafe {
        let count = attrs.GetCount().unwrap_or(0);
        let mut keys = Vec::new();
        for i in 0..count {
            let mut key = GUID::zeroed();
            if attrs.GetItemByIndex(i, &mut key, None).is_ok() {
                keys.push(format!("{key:?}"));
            }
        }
        log!("{what}: {count} attributes {keys:?}");
    }
}

impl IMFMediaEventGenerator_Impl for MediaSource_Impl {
    fn GetEvent(
        &self,
        flags: MEDIA_EVENT_GENERATOR_GET_EVENT_FLAGS,
    ) -> windows_core::Result<IMFMediaEvent> {
        guard("Source::GetEvent", || {
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
        guard("Source::BeginGetEvent", || {
            let queue = self.queue()?;
            // SAFETY: delegates to a valid event queue, taken out of the lock first since
            // GetEvent may block.
            unsafe { queue.BeginGetEvent(callback.as_ref(), state.as_ref()) }
        })
    }

    fn EndGetEvent(&self, result: Ref<IMFAsyncResult>) -> windows_core::Result<IMFMediaEvent> {
        guard("Source::EndGetEvent", || {
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
        guard("Source::QueueEvent", || {
            let queue = self.queue()?;
            // SAFETY: delegates to a valid event queue, taken out of the lock first since
            // GetEvent may block.
            unsafe { queue.QueueEventParamVar(met, extended, status, value) }
        })
    }
}

impl IMFMediaSource_Impl for MediaSource_Impl {
    fn GetCharacteristics(&self) -> windows_core::Result<u32> {
        guard("Source::GetCharacteristics", || {
            self.queue()?;
            Ok(MFMEDIASOURCE_IS_LIVE.0 as u32)
        })
    }

    fn CreatePresentationDescriptor(&self) -> windows_core::Result<IMFPresentationDescriptor> {
        guard("Source::CreatePresentationDescriptor", || {
            let inner = self.lock();
            inner.queue.as_ref().ok_or(MF_E_SHUTDOWN)?;
            // SAFETY: cloning a valid presentation descriptor.
            unsafe { inner.descriptor.Clone() }
        })
    }

    fn Start(
        &self,
        descriptor: Ref<IMFPresentationDescriptor>,
        time_format: *const GUID,
        _start: *const PROPVARIANT,
    ) -> windows_core::Result<()> {
        guard("Source::Start", || {
            // SAFETY: a non-null time format pointer points at a GUID.
            if !time_format.is_null() && unsafe { *time_format } != GUID::zeroed() {
                return Err(MF_E_UNSUPPORTED_TIME_FORMAT.into());
            }
            let pd = descriptor.as_ref().ok_or(E_POINTER)?;
            let mut inner = self.lock();
            let queue = inner.queue.clone().ok_or(MF_E_SHUTDOWN)?;
            let stream = inner.stream.clone().ok_or(MF_E_SHUTDOWN)?;

            // Frame Server may only set the device link after activation.
            if inner.camera.is_none() {
                inner.camera = resolve_camera(&self.activate_attributes, Some(&inner.attributes));
                if inner.camera.is_none() {
                    log_attribute_keys("activate attributes", &self.activate_attributes);
                    log_attribute_keys("source attributes", &inner.attributes);
                }
            }
            stream.set_camera(inner.camera.as_ref().map(|c| c.id));

            let mut selected = BOOL::default();
            let mut sd = None;
            // SAFETY: index 0 exists (one stream); out-params are valid.
            unsafe { pd.GetStreamDescriptorByIndex(0, &mut selected, &mut sd)? };
            let sd = sd.ok_or(E_POINTER)?;
            let first = !inner.started_once;
            inner.started_once = true;
            drop(inner);

            if selected.as_bool() {
                stream.start(&sd)?;
                let event = if first { MENewStream } else { MEUpdatedStream };
                let unknown: IUnknown = stream::as_interface(&stream).cast()?;
                // SAFETY: valid queue and stream object.
                unsafe {
                    queue.QueueEventParamUnk(
                        event.0 as u32,
                        &GUID::zeroed(),
                        HRESULT(0),
                        &unknown,
                    )?
                };
            } else {
                stream.stop()?;
            }
            queue_time_event(&queue, MESourceStarted.0 as u32)
        })
    }

    fn Stop(&self) -> windows_core::Result<()> {
        guard("Source::Stop", || {
            let (queue, stream) = {
                let inner = self.lock();
                (
                    inner.queue.clone().ok_or(MF_E_SHUTDOWN)?,
                    inner.stream.clone(),
                )
            };
            if let Some(stream) = stream {
                stream.stop()?;
            }
            queue_time_event(&queue, MESourceStopped.0 as u32)
        })
    }

    fn Pause(&self) -> windows_core::Result<()> {
        guard("Source::Pause", || {
            self.queue()?;
            Err(MF_E_INVALID_STATE_TRANSITION.into())
        })
    }

    fn Shutdown(&self) -> windows_core::Result<()> {
        guard("Source::Shutdown", || {
            let (queue, stream) = {
                let mut inner = self.lock();
                (inner.queue.take(), inner.stream.take())
            };
            if let Some(stream) = stream {
                stream.shutdown();
            }
            if let Some(queue) = queue {
                // SAFETY: valid queue.
                unsafe { queue.Shutdown()? };
                log!("media source shut down");
            }
            Ok(())
        })
    }
}

impl IMFMediaSourceEx_Impl for MediaSource_Impl {
    fn GetSourceAttributes(&self) -> windows_core::Result<IMFAttributes> {
        guard("Source::GetSourceAttributes", || {
            let inner = self.lock();
            inner.queue.as_ref().ok_or(MF_E_SHUTDOWN)?;
            Ok(inner.attributes.clone())
        })
    }

    fn GetStreamAttributes(&self, id: u32) -> windows_core::Result<IMFAttributes> {
        guard("Source::GetStreamAttributes", || {
            let stream = self.lock().stream.clone().ok_or(MF_E_SHUTDOWN)?;
            if id != 0 {
                return Err(MF_E_INVALIDSTREAMNUMBER.into());
            }
            stream::stream_attributes(&stream)
        })
    }

    fn SetD3DManager(&self, _manager: Ref<IUnknown>) -> windows_core::Result<()> {
        // System-memory samples only for now (the D3D path is Phase 8).
        guard("Source::SetD3DManager", || self.queue().map(|_| ()))
    }
}

impl IMFGetService_Impl for MediaSource_Impl {
    fn GetService(
        &self,
        _service: *const GUID,
        _iid: *const GUID,
        _out: *mut *mut core::ffi::c_void,
    ) -> windows_core::Result<()> {
        Err(MF_E_UNSUPPORTED_SERVICE.into())
    }
}

impl IKsControl_Impl for MediaSource_Impl {
    fn KsProperty(
        &self,
        _: *const KSIDENTIFIER,
        _: u32,
        _: *mut core::ffi::c_void,
        _: u32,
        _: *mut u32,
    ) -> windows_core::Result<()> {
        Err(ERROR_SET_NOT_FOUND.to_hresult().into())
    }

    fn KsMethod(
        &self,
        _: *const KSIDENTIFIER,
        _: u32,
        _: *mut core::ffi::c_void,
        _: u32,
        _: *mut u32,
    ) -> windows_core::Result<()> {
        Err(ERROR_SET_NOT_FOUND.to_hresult().into())
    }

    fn KsEvent(
        &self,
        _: *const KSIDENTIFIER,
        _: u32,
        _: *mut core::ffi::c_void,
        _: u32,
        _: *mut u32,
    ) -> windows_core::Result<()> {
        Err(ERROR_SET_NOT_FOUND.to_hresult().into())
    }
}
