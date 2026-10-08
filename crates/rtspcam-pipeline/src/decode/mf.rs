//! Media Foundation decoder MFTs (H.264, H.265, MJPEG) in synchronous software mode.
//!
//! The decoder is found with `MFTEnumEx`, given the compressed input type, and asked for NV12
//! output (or I420/YV12/YUY2, converted to NV12 on the CPU). Resolution changes in the stream
//! (`MF_E_TRANSFORM_STREAM_CHANGE`) renegotiate the output type.

use std::mem::ManuallyDrop;
use std::time::Duration;

use windows::Win32::Foundation::RPC_E_CHANGED_MODE;
use windows::Win32::Media::MediaFoundation::{
    IMFActivate, IMFMediaType, IMFSample, IMFTransform, MF_E_NOTACCEPTING,
    MF_E_TRANSFORM_NEED_MORE_INPUT, MF_E_TRANSFORM_STREAM_CHANGE, MF_E_TRANSFORM_TYPE_NOT_SET,
    MF_LOW_LATENCY, MF_MT_DEFAULT_STRIDE, MF_MT_FRAME_SIZE, MF_MT_INTERLACE_MODE, MF_MT_MAJOR_TYPE,
    MF_MT_MINIMUM_DISPLAY_APERTURE, MF_MT_SUBTYPE, MF_VERSION, MFCreateMediaType,
    MFCreateMemoryBuffer, MFCreateSample, MFMediaType_Video, MFSTARTUP_NOSOCKET, MFShutdown,
    MFStartup, MFT_CATEGORY_VIDEO_DECODER, MFT_ENUM_FLAG, MFT_ENUM_FLAG_LOCALMFT,
    MFT_ENUM_FLAG_SORTANDFILTER, MFT_ENUM_FLAG_SYNCMFT, MFT_FRIENDLY_NAME_Attribute,
    MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, MFT_MESSAGE_NOTIFY_START_OF_STREAM, MFT_OUTPUT_DATA_BUFFER,
    MFT_OUTPUT_STREAM_CAN_PROVIDE_SAMPLES, MFT_OUTPUT_STREAM_PROVIDES_SAMPLES,
    MFT_REGISTER_TYPE_INFO, MFTEnumEx, MFVideoArea, MFVideoFormat_H264, MFVideoFormat_HEVC,
    MFVideoFormat_I420, MFVideoFormat_IYUV, MFVideoFormat_MJPG, MFVideoFormat_NV12,
    MFVideoFormat_YUY2, MFVideoFormat_YV12, MFVideoInterlace_Progressive,
};
use windows::Win32::System::Com::{
    COINIT_MULTITHREADED, CoInitializeEx, CoTaskMemFree, CoUninitialize,
};
use windows_core::GUID;

use super::{ArrivalLog, Decoder, stamp};
use crate::error::{ErrorKind, PipelineError};
use crate::frame::{Chroma, Yuv420Planes, yuy2_to_frame};
use crate::{EncodedFrame, Frame, VideoCodec};

/// Output pixel formats we can turn into NV12, best first.
const OUTPUT_PREFERENCE: [GUID; 5] = [
    MFVideoFormat_NV12,
    MFVideoFormat_I420,
    MFVideoFormat_IYUV,
    MFVideoFormat_YV12,
    MFVideoFormat_YUY2,
];

/// Keeps COM and Media Foundation initialized on this thread for the decoder's lifetime.
struct MfRuntime {
    com_initialized: bool,
}

impl MfRuntime {
    fn start() -> Result<Self, PipelineError> {
        // SAFETY: plain COM initialization on the current thread. If the thread is already in
        // an STA we keep using it (COM objects work there too) and don't uninitialize.
        let hr = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        let com_initialized = hr.is_ok();
        if hr.is_err() && hr != RPC_E_CHANGED_MODE {
            return Err(PipelineError::new(
                ErrorKind::DecoderUnavailable,
                format!("COM initialization failed: {hr}"),
            ));
        }
        // SAFETY: MFStartup is reference counted and paired with MFShutdown in Drop.
        if let Err(e) = unsafe { MFStartup(MF_VERSION, MFSTARTUP_NOSOCKET) } {
            if com_initialized {
                // SAFETY: balances the successful CoInitializeEx above.
                unsafe { CoUninitialize() };
            }
            return Err(PipelineError::new(
                ErrorKind::DecoderUnavailable,
                format!("Media Foundation is not available: {e}"),
            ));
        }
        Ok(Self { com_initialized })
    }
}

impl Drop for MfRuntime {
    fn drop(&mut self) {
        // SAFETY: balances MFStartup / CoInitializeEx from `start`, on the same thread
        // (decoders are not Send).
        unsafe {
            let _ = MFShutdown();
            if self.com_initialized {
                CoUninitialize();
            }
        }
    }
}

/// The negotiated output format.
#[derive(Debug, Clone, Copy)]
struct OutputFormat {
    subtype: GUID,
    /// Buffer size in pixels (may include padding rows, for example 1088 for 1080p H.264).
    coded: (u32, u32),
    /// Bytes per row of the first plane, if the media type says.
    stride: Option<usize>,
    /// Visible area: x, y, width, height.
    visible: (u32, u32, u32, u32),
}

pub struct MfDecoder {
    // Field order matters: COM objects must be released before Media Foundation shuts down.
    transform: IMFTransform,
    name: String,
    output: Option<OutputFormat>,
    /// Our own output sample, when the MFT doesn't allocate them.
    out_sample: Option<(IMFSample, u32)>,
    mft_provides_samples: bool,
    arrivals: ArrivalLog,
    _runtime: MfRuntime,
}

impl std::fmt::Debug for MfDecoder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MfDecoder")
            .field("name", &self.name)
            .field("output", &self.output)
            .finish_non_exhaustive()
    }
}

impl MfDecoder {
    pub fn new(codec: VideoCodec, size: Option<(u32, u32)>) -> Result<Self, PipelineError> {
        let runtime = MfRuntime::start()?;
        let subtype = match codec {
            VideoCodec::H264 => MFVideoFormat_H264,
            VideoCodec::H265 => MFVideoFormat_HEVC,
            VideoCodec::Mjpeg => MFVideoFormat_MJPG,
        };
        let (transform, mft_name) = find_decoder(subtype).map_err(|e| match e {
            Some(e) => PipelineError::new(
                ErrorKind::DecoderUnavailable,
                format!("couldn't start the Media Foundation {codec} decoder: {e}"),
            ),
            None => PipelineError::new(
                ErrorKind::DecoderUnavailable,
                format!("no Media Foundation {codec} decoder is installed"),
            ),
        })?;

        // SAFETY: the transform is a valid MFT; all calls below follow the MFT protocol
        // (attributes, input type, output type, then streaming messages).
        unsafe {
            // Ask for output as soon as a picture is decodable instead of buffering a GOP.
            if let Ok(attrs) = transform.GetAttributes() {
                let _ = attrs.SetUINT32(&MF_LOW_LATENCY, 1);
            }

            let input = MFCreateMediaType()?;
            input.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
            input.SetGUID(&MF_MT_SUBTYPE, &subtype)?;
            input.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
            if let Some((w, h)) = size {
                input.SetUINT64(&MF_MT_FRAME_SIZE, (u64::from(w) << 32) | u64::from(h))?;
            }
            transform.SetInputType(0, &input, 0).map_err(|e| {
                PipelineError::new(
                    ErrorKind::DecoderUnavailable,
                    format!("{mft_name} rejected the {codec} input format: {e}"),
                )
            })?;
        }

        let mut decoder = Self {
            transform,
            name: format!("Media Foundation ({mft_name})"),
            output: None,
            out_sample: None,
            mft_provides_samples: false,
            arrivals: ArrivalLog::default(),
            _runtime: runtime,
        };
        // Some decoders only know their output type after seeing the stream.
        match decoder.negotiate_output() {
            Ok(()) => {}
            Err(e) if e.code() == MF_E_TRANSFORM_TYPE_NOT_SET => {}
            Err(e) => return Err(e.into()),
        }
        // SAFETY: valid transform; these notifications have no parameters.
        unsafe {
            decoder
                .transform
                .ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)?;
            decoder
                .transform
                .ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)?;
        }
        Ok(decoder)
    }

    /// Picks the best output type the decoder offers and records its geometry.
    fn negotiate_output(&mut self) -> windows_core::Result<()> {
        // SAFETY: valid transform; media types returned by the MFT are owned COM references.
        unsafe {
            let mut offered = Vec::new();
            for i in 0.. {
                match self.transform.GetOutputAvailableType(0, i) {
                    Ok(t) => offered.push(t),
                    Err(e) if offered.is_empty() => return Err(e),
                    Err(_) => break,
                }
            }
            let chosen = OUTPUT_PREFERENCE
                .iter()
                .find_map(|want| {
                    offered
                        .iter()
                        .find(|t| t.GetGUID(&MF_MT_SUBTYPE).is_ok_and(|s| s == *want))
                })
                .ok_or_else(|| {
                    windows_core::Error::new(
                        MF_E_TRANSFORM_TYPE_NOT_SET,
                        "decoder offers no NV12/I420/YV12/YUY2 output",
                    )
                })?;
            self.transform.SetOutputType(0, chosen, 0)?;
            let current = self.transform.GetOutputCurrentType(0)?;
            self.output = Some(read_format(&current)?);

            let info = self.transform.GetOutputStreamInfo(0)?;
            self.mft_provides_samples = info.dwFlags
                & (MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0 | MFT_OUTPUT_STREAM_CAN_PROVIDE_SAMPLES.0)
                    as u32
                != 0;
            if !self.mft_provides_samples {
                let size = info.cbSize.max(1);
                if self.out_sample.as_ref().is_none_or(|(_, s)| *s < size) {
                    let sample = MFCreateSample()?;
                    sample.AddBuffer(&MFCreateMemoryBuffer(size)?)?;
                    self.out_sample = Some((sample, size));
                }
            }
        }
        tracing::debug!(decoder = %self.name, output = ?self.output, "output format negotiated");
        Ok(())
    }

    /// Pulls every picture the decoder has ready.
    fn drain(&mut self, out: &mut Vec<Frame>) -> Result<(), PipelineError> {
        loop {
            if self.output.is_none() {
                match self.negotiate_output() {
                    Ok(()) => {}
                    // Not enough input yet to know the format.
                    Err(e) if e.code() == MF_E_TRANSFORM_TYPE_NOT_SET => return Ok(()),
                    Err(e) => return Err(e.into()),
                }
            }
            let provided = if self.mft_provides_samples {
                None
            } else {
                self.out_sample.as_ref().map(|(s, _)| {
                    // The H.264 MFT fails with "CopyDecodedFrame failed" (E_FAIL) if the reused
                    // buffer still holds the previous picture's length.
                    // SAFETY: our own sample, created with exactly one buffer.
                    let _ = unsafe { s.GetBufferByIndex(0).and_then(|b| b.SetCurrentLength(0)) };
                    s.clone()
                })
            };
            let mut buffer = MFT_OUTPUT_DATA_BUFFER {
                dwStreamID: 0,
                pSample: ManuallyDrop::new(provided),
                dwStatus: 0,
                pEvents: ManuallyDrop::new(None),
            };
            let mut status = 0;
            // SAFETY: one output buffer for stream 0, as the MFT expects. We take ownership of
            // the sample and events references back right after the call.
            let result = unsafe {
                self.transform
                    .ProcessOutput(0, std::slice::from_mut(&mut buffer), &mut status)
            };
            let sample = ManuallyDrop::into_inner(buffer.pSample);
            drop(ManuallyDrop::into_inner(buffer.pEvents));
            match result {
                Ok(()) => {
                    if let Some(sample) = sample
                        && let Some(frame) = self.convert(&sample)?
                    {
                        out.push(frame);
                    }
                }
                Err(e) if e.code() == MF_E_TRANSFORM_NEED_MORE_INPUT => return Ok(()),
                Err(e) if e.code() == MF_E_TRANSFORM_STREAM_CHANGE => {
                    self.output = None;
                    self.negotiate_output()?;
                }
                Err(e) => return Err(e.into()),
            }
        }
    }

    /// Copies a decoded sample into an NV12 [`Frame`].
    fn convert(&self, sample: &IMFSample) -> Result<Option<Frame>, PipelineError> {
        let Some(fmt) = self.output else {
            return Ok(None);
        };
        // SAFETY: the sample comes from ProcessOutput. The locked pointer is valid for
        // `len` bytes until Unlock, and the slice built from it doesn't outlive the lock.
        unsafe {
            let pts = sample.GetSampleTime().map_or(Duration::ZERO, |t| {
                Duration::from_nanos(t.max(0) as u64 * 100)
            });
            let buffer = sample.ConvertToContiguousBuffer()?;
            let mut ptr = std::ptr::null_mut();
            let mut len = 0u32;
            buffer.Lock(&mut ptr, None, Some(&mut len))?;
            let data = std::slice::from_raw_parts(ptr, len as usize);
            let frame = picture_to_frame(data, fmt);
            buffer.Unlock()?;
            Ok(frame.map(|f| stamp(f, pts, &self.arrivals)))
        }
    }
}

impl Decoder for MfDecoder {
    fn name(&self) -> &str {
        &self.name
    }

    fn decode(&mut self, frame: &EncodedFrame, out: &mut Vec<Frame>) -> Result<(), PipelineError> {
        self.arrivals.push(frame.pts, frame.received);
        // SAFETY: a fresh sample with one memory buffer; the locked pointer is valid for the
        // buffer's max length (at least `frame.data.len()`) until Unlock.
        let sample = unsafe {
            let len = u32::try_from(frame.data.len())
                .map_err(|_| PipelineError::new(ErrorKind::Decode, "frame too large"))?;
            let buffer = MFCreateMemoryBuffer(len.max(1))?;
            let mut ptr = std::ptr::null_mut();
            buffer.Lock(&mut ptr, None, None)?;
            std::ptr::copy_nonoverlapping(frame.data.as_ptr(), ptr, frame.data.len());
            buffer.Unlock()?;
            buffer.SetCurrentLength(len)?;
            let sample = MFCreateSample()?;
            sample.AddBuffer(&buffer)?;
            sample.SetSampleTime(i64::try_from(frame.pts.as_nanos() / 100).unwrap_or(i64::MAX))?;
            sample
        };
        // A decoder that is full must be drained before it takes more input.
        for _ in 0..3 {
            // SAFETY: valid transform and sample.
            match unsafe { self.transform.ProcessInput(0, &sample, 0) } {
                Ok(()) => return self.drain(out),
                Err(e) if e.code() == MF_E_NOTACCEPTING => self.drain(out)?,
                Err(e) => {
                    return Err(PipelineError::new(
                        ErrorKind::Decode,
                        format!("{} rejected a frame: {e}", self.name),
                    ));
                }
            }
        }
        Err(PipelineError::new(
            ErrorKind::Decode,
            format!("{} stopped accepting input", self.name),
        ))
    }
}

/// Finds and activates the first synchronous decoder MFT for `subtype`.
/// `Err(None)` means none is installed.
fn find_decoder(subtype: GUID) -> Result<(IMFTransform, String), Option<windows_core::Error>> {
    let input = MFT_REGISTER_TYPE_INFO {
        guidMajorType: MFMediaType_Video,
        guidSubtype: subtype,
    };
    let flags = MFT_ENUM_FLAG(
        MFT_ENUM_FLAG_SYNCMFT.0 | MFT_ENUM_FLAG_LOCALMFT.0 | MFT_ENUM_FLAG_SORTANDFILTER.0,
    );
    let mut activates: *mut Option<IMFActivate> = std::ptr::null_mut();
    let mut count = 0u32;
    // SAFETY: MFTEnumEx fills a CoTaskMemAlloc'ed array of `count` activation objects. We
    // take each reference out of the array (so it's released exactly once) and then free it.
    let list: Vec<IMFActivate> = unsafe {
        MFTEnumEx(
            MFT_CATEGORY_VIDEO_DECODER,
            flags,
            Some(&input),
            None,
            &mut activates,
            &mut count,
        )
        .map_err(Some)?;
        let list = if activates.is_null() {
            Vec::new()
        } else {
            let items = std::slice::from_raw_parts_mut(activates, count as usize);
            items.iter_mut().filter_map(Option::take).collect()
        };
        CoTaskMemFree(Some(activates as *const _));
        list
    };
    let mut last_err = None;
    for activate in list {
        // SAFETY: valid activation object from MFTEnumEx.
        match unsafe { activate.ActivateObject::<IMFTransform>() } {
            Ok(t) => return Ok((t, friendly_name(&activate))),
            Err(e) => last_err = Some(e),
        }
    }
    Err(last_err)
}

fn friendly_name(activate: &IMFActivate) -> String {
    let mut buf = [0u16; 256];
    let mut len = 0u32;
    // SAFETY: GetString writes at most `buf.len()` UTF-16 units including the terminator.
    match unsafe { activate.GetString(&MFT_FRIENDLY_NAME_Attribute, &mut buf, Some(&mut len)) } {
        Ok(()) => String::from_utf16_lossy(&buf[..len as usize]),
        Err(_) => "decoder MFT".to_owned(),
    }
}

fn read_format(t: &IMFMediaType) -> windows_core::Result<OutputFormat> {
    // SAFETY: valid media type; GetBlob writes at most size_of::<MFVideoArea>() bytes into
    // `area`, which is plain old data.
    unsafe {
        let subtype = t.GetGUID(&MF_MT_SUBTYPE)?;
        // Some decoders (HEVC) only know the size once the stream starts; until then the
        // picture size is 0x0, no pictures are produced, and a stream change follows.
        let size = t.GetUINT64(&MF_MT_FRAME_SIZE).unwrap_or(0);
        let coded = ((size >> 32) as u32, size as u32);
        // Stored as UINT32 but holds a signed value (negative = bottom-up, not used here).
        let stride = t
            .GetUINT32(&MF_MT_DEFAULT_STRIDE)
            .ok()
            .map(|s| s as i32)
            .filter(|s| *s > 0)
            .map(|s| s as usize);
        let mut area = MFVideoArea::default();
        let bytes = std::slice::from_raw_parts_mut(
            (&raw mut area).cast::<u8>(),
            std::mem::size_of::<MFVideoArea>(),
        );
        let visible = match t.GetBlob(&MF_MT_MINIMUM_DISPLAY_APERTURE, bytes, None) {
            Ok(()) if area.Area.cx > 0 && area.Area.cy > 0 => (
                area.OffsetX.value.max(0) as u32,
                area.OffsetY.value.max(0) as u32,
                area.Area.cx as u32,
                area.Area.cy as u32,
            ),
            _ => (0, 0, coded.0, coded.1),
        };
        Ok(OutputFormat {
            subtype,
            coded,
            stride,
            visible,
        })
    }
}

/// Crops and converts one decoded picture to NV12.
fn picture_to_frame(data: &[u8], fmt: OutputFormat) -> Option<Frame> {
    let (cw, ch) = (fmt.coded.0 as usize, fmt.coded.1 as usize);
    let (vx, vy, vw, vh) = fmt.visible;
    let (vx, vy) = (vx as usize & !1, vy as usize & !1);
    if cw == 0 || ch == 0 {
        return None;
    }
    if fmt.subtype == MFVideoFormat_YUY2 {
        let stride = fmt.stride.unwrap_or(data.len() / ch);
        let start = vy * stride + vx * 2;
        return yuy2_to_frame(data.get(start..)?, stride, vw, vh);
    }
    // 4:2:0 formats: derive the stride from the buffer if the media type didn't say.
    let stride = fmt.stride.unwrap_or(data.len() * 2 / (ch * 3));
    let y_size = stride * ch;
    let y = data.get(vy * stride + vx..)?;
    let chroma = if fmt.subtype == MFVideoFormat_NV12 {
        Chroma::Interleaved {
            uv: data.get(y_size + (vy / 2) * stride + vx..)?,
            stride,
        }
    } else {
        let c_stride = stride / 2;
        let c_size = c_stride * (ch / 2);
        let offset = (vy / 2) * c_stride + vx / 2;
        let first = data.get(y_size + offset..)?;
        let second = data.get(y_size + c_size + offset..)?;
        let (u, v) = if fmt.subtype == MFVideoFormat_YV12 {
            (second, first)
        } else {
            (first, second)
        };
        Chroma::Planar {
            u,
            v,
            stride: c_stride,
        }
    };
    Yuv420Planes {
        y,
        y_stride: stride,
        chroma,
        width: vw,
        height: vh,
    }
    .to_frame()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crops_nv12_aperture() {
        // 4x4 coded, 4x2 visible, stride 6.
        let mut data = vec![0u8; 6 * 4 * 3 / 2];
        data[..4].copy_from_slice(&[1, 2, 3, 4]);
        data[6..10].copy_from_slice(&[5, 6, 7, 8]);
        data[24..28].copy_from_slice(&[100, 101, 102, 103]);
        let fmt = OutputFormat {
            subtype: MFVideoFormat_NV12,
            coded: (4, 4),
            stride: Some(6),
            visible: (0, 0, 4, 2),
        };
        let f = picture_to_frame(&data, fmt).unwrap();
        assert_eq!((f.width(), f.height()), (4, 2));
        assert_eq!(f.y(), &[1, 2, 3, 4, 5, 6, 7, 8]);
        assert_eq!(f.uv(), &[100, 101, 102, 103]);
    }

    #[test]
    fn derives_stride_and_swaps_yv12() {
        // 4x2 YV12 without a stride attribute: Y (8), V (2), U (2).
        let data = [0, 0, 0, 0, 0, 0, 0, 0, 20, 21, 10, 11];
        let fmt = OutputFormat {
            subtype: MFVideoFormat_YV12,
            coded: (4, 2),
            stride: None,
            visible: (0, 0, 4, 2),
        };
        let f = picture_to_frame(&data, fmt).unwrap();
        assert_eq!(f.uv(), &[10, 20, 11, 21]);
    }
}
