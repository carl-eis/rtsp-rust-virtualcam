//! Direct3D 11 for hardware decoding: finding the graphics card (the probe), the device the
//! decoders share, and getting decoded pictures out of GPU memory (the copy-back), shrunk on
//! the GPU first when consumers need less than the full size.
//!
//! One device serves every decoder, on the default adapter (the one Windows' per-app graphics
//! preference picks), and that adapter is what Settings shows. Its multithread protection is
//! on, as Media Foundation requires when decoders on several threads share it; our own
//! sequences of calls (blit, copy, map) also hold its lock so they don't interleave.

use std::collections::HashMap;
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{CloseHandle, E_FAIL, HANDLE, HMODULE, RECT};
use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_UNKNOWN;
use windows::Win32::Graphics::Direct3D11::{
    D3D11_BIND_RENDER_TARGET, D3D11_BOX, D3D11_CPU_ACCESS_READ, D3D11_CREATE_DEVICE_VIDEO_SUPPORT,
    D3D11_DECODER_PROFILE_H264_VLD_NOFGT, D3D11_DECODER_PROFILE_HEVC_VLD_MAIN,
    D3D11_FENCE_FLAG_NONE, D3D11_MAP_READ, D3D11_MAPPED_SUBRESOURCE, D3D11_SDK_VERSION,
    D3D11_TEX2D_VPIV, D3D11_TEX2D_VPOV, D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT,
    D3D11_USAGE_STAGING, D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE, D3D11_VIDEO_PROCESSOR_COLOR_SPACE,
    D3D11_VIDEO_PROCESSOR_CONTENT_DESC, D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC,
    D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC_0, D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC,
    D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC_0, D3D11_VIDEO_PROCESSOR_STREAM,
    D3D11_VIDEO_USAGE_OPTIMAL_SPEED, D3D11_VPIV_DIMENSION_TEXTURE2D,
    D3D11_VPOV_DIMENSION_TEXTURE2D, D3D11CreateDevice, ID3D11Device, ID3D11Device5,
    ID3D11DeviceContext, ID3D11DeviceContext4, ID3D11Fence, ID3D11Multithread, ID3D11Texture2D,
    ID3D11VideoContext, ID3D11VideoDevice, ID3D11VideoProcessor, ID3D11VideoProcessorEnumerator,
    ID3D11VideoProcessorInputView, ID3D11VideoProcessorOutputView,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_NV12, DXGI_RATIONAL, DXGI_SAMPLE_DESC};
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory1, DXGI_ADAPTER_FLAG_SOFTWARE, IDXGIFactory1,
};
use windows::Win32::Media::MediaFoundation::{IMFDXGIDeviceManager, MFCreateDXGIDeviceManager};
use windows::Win32::System::Threading::{CreateEventW, WaitForSingleObject};
use windows_core::Interface as _;

use rtspcam_core::profile::{self, Stage};
use rtspcam_pipeline::decode::{HardwareDecoding, limited_size};
use rtspcam_pipeline::frame::{Chroma, Yuv420Planes};
use rtspcam_pipeline::{Frame, VideoCodec};

/// How long a decoder waits for the probe that runs at startup before it decodes in software.
const PROBE_WAIT: Duration = Duration::from_secs(2);

/// The shared Direct3D 11 device, with what decoders need from it.
pub(crate) struct Gpu {
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    video_device: ID3D11VideoDevice,
    video_context: ID3D11VideoContext,
    multithread: ID3D11Multithread,
    /// Handed to decoder MFTs (`MFT_MESSAGE_SET_D3D_MANAGER`).
    pub(crate) manager: IMFDXGIDeviceManager,
    /// The adapter's name, for Settings and the log.
    pub(crate) adapter: String,
    /// The codecs the adapter decodes.
    pub(crate) codecs: Vec<VideoCodec>,
}

// SAFETY: the device is created with multithread protection on, which makes the immediate
// context (and the video context, which is the same object) safe to call from any thread;
// ID3D11Device and IMFDXGIDeviceManager are free-threaded. Sequences of context calls that
// must not interleave hold `lock()`.
unsafe impl Send for Gpu {}
// SAFETY: see `Send`.
unsafe impl Sync for Gpu {}

impl std::fmt::Debug for Gpu {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Gpu")
            .field("adapter", &self.adapter)
            .field("codecs", &self.codecs)
            .finish_non_exhaustive()
    }
}

/// Holds the device's multithread lock.
struct GpuLock<'a>(&'a ID3D11Multithread);

impl Drop for GpuLock<'_> {
    fn drop(&mut self) {
        // SAFETY: paired with the Enter in `Gpu::lock`.
        unsafe { self.0.Leave() };
    }
}

fn hr(e: windows_core::Error) -> String {
    e.message()
}

impl Gpu {
    /// Opens the default adapter for video decoding. `Err` is the reason, for the user.
    pub(crate) fn probe() -> Result<Self, String> {
        // SAFETY: plain DXGI and D3D11 creation calls; every out-pointer is a local that lives
        // across the call, and the returned interfaces are owned references.
        unsafe {
            let factory: IDXGIFactory1 =
                CreateDXGIFactory1().map_err(|e| format!("DXGI is not available: {}", hr(e)))?;
            let adapter = factory
                .EnumAdapters1(0)
                .map_err(|_| "No graphics card found".to_owned())?;
            let desc = adapter
                .GetDesc1()
                .map_err(|e| format!("Could not read the graphics card: {}", hr(e)))?;
            let name_len = desc.Description.iter().position(|&c| c == 0).unwrap_or(128);
            let adapter_name = String::from_utf16_lossy(&desc.Description[..name_len])
                .trim()
                .to_owned();
            if desc.Flags & DXGI_ADAPTER_FLAG_SOFTWARE.0 as u32 != 0 {
                return Err("No graphics card with video decoding found".to_owned());
            }

            let mut device = None;
            let mut context = None;
            D3D11CreateDevice(
                &adapter,
                D3D_DRIVER_TYPE_UNKNOWN,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_VIDEO_SUPPORT,
                None,
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                Some(&mut context),
            )
            .map_err(|e| format!("{adapter_name} can't decode video: {}", hr(e)))?;
            let (Some(device), Some(context)) = (device, context) else {
                return Err(format!("{adapter_name} can't decode video"));
            };
            let multithread: ID3D11Multithread = context.cast().map_err(hr)?;
            let _ = multithread.SetMultithreadProtected(true);
            let video_device: ID3D11VideoDevice = device
                .cast()
                .map_err(|_| format!("{adapter_name} has no video decoder"))?;
            let video_context: ID3D11VideoContext = context.cast().map_err(hr)?;

            let profiles: Vec<_> = (0..video_device.GetVideoDecoderProfileCount())
                .filter_map(|i| video_device.GetVideoDecoderProfile(i).ok())
                .collect();
            let mut codecs = Vec::new();
            if profiles.contains(&D3D11_DECODER_PROFILE_H264_VLD_NOFGT) {
                codecs.push(VideoCodec::H264);
            }
            if profiles.contains(&D3D11_DECODER_PROFILE_HEVC_VLD_MAIN) {
                codecs.push(VideoCodec::H265);
            }
            if codecs.is_empty() {
                return Err(format!("{adapter_name} can't decode H.264 or H.265"));
            }

            let mut token = 0;
            let mut manager = None;
            MFCreateDXGIDeviceManager(&mut token, &mut manager)
                .map_err(|e| format!("Media Foundation can't use the graphics card: {}", hr(e)))?;
            let manager = manager.ok_or("Media Foundation can't use the graphics card")?;
            manager
                .ResetDevice(&device, token)
                .map_err(|e| format!("Media Foundation can't use the graphics card: {}", hr(e)))?;

            Ok(Self {
                device,
                context,
                video_device,
                video_context,
                multithread,
                manager,
                adapter: adapter_name,
                codecs,
            })
        }
    }

    /// Whether the device was removed or reset (driver update, GPU reset, unplugged).
    pub(crate) fn is_lost(&self) -> bool {
        // SAFETY: a plain query on a valid device.
        unsafe { self.device.GetDeviceRemovedReason().is_err() }
    }

    fn lock(&self) -> GpuLock<'_> {
        // SAFETY: paired with Leave in GpuLock's Drop.
        unsafe { self.multithread.Enter() };
        GpuLock(&self.multithread)
    }

    fn texture(
        &self,
        width: u32,
        height: u32,
        staging: bool,
    ) -> windows_core::Result<ID3D11Texture2D> {
        let desc = D3D11_TEXTURE2D_DESC {
            Width: width,
            Height: height,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_NV12,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Usage: if staging {
                D3D11_USAGE_STAGING
            } else {
                D3D11_USAGE_DEFAULT
            },
            BindFlags: if staging {
                0
            } else {
                D3D11_BIND_RENDER_TARGET.0 as u32
            },
            CPUAccessFlags: if staging {
                D3D11_CPU_ACCESS_READ.0 as u32
            } else {
                0
            },
            MiscFlags: 0,
        };
        let mut texture = None;
        // SAFETY: a valid description; the out-pointer is a local.
        unsafe {
            self.device
                .CreateTexture2D(&desc, None, Some(&mut texture))?
        };
        texture.ok_or_else(|| windows_core::Error::from(E_FAIL))
    }
}

/// The hardware decoding state of the process: the switch, and the probe's result.
#[derive(Debug)]
pub(crate) struct Hardware {
    state: Mutex<HwState>,
    probed: Condvar,
}

#[derive(Debug)]
struct HwState {
    on: bool,
    probe: Probe,
}

#[derive(Debug, Clone)]
enum Probe {
    Running,
    Ready(Arc<Gpu>),
    Failed(String),
    /// The device was lost; the next decoder probes again.
    Lost,
}

impl Hardware {
    /// Starts the probe on a background thread (creating the device can take a few hundred
    /// milliseconds). Hardware decoding stays off until [`set_on`](Self::set_on).
    pub(crate) fn start() -> Arc<Self> {
        let hw = Arc::new(Self {
            state: Mutex::new(HwState {
                on: false,
                probe: Probe::Running,
            }),
            probed: Condvar::new(),
        });
        let worker = hw.clone();
        let spawned = std::thread::Builder::new()
            .name("gpu probe".into())
            .spawn(move || worker.finish_probe(Gpu::probe()));
        if let Err(e) = spawned {
            hw.finish_probe(Err(format!("Could not check the graphics card: {e}")));
        }
        hw
    }

    fn finish_probe(&self, result: Result<Gpu, String>) {
        let probe = match result {
            Ok(gpu) => {
                tracing::info!(
                    "hardware decoding: {}",
                    HardwareDecoding::Available {
                        device: gpu.adapter.clone(),
                        codecs: gpu.codecs.clone()
                    }
                    .describe()
                );
                Probe::Ready(Arc::new(gpu))
            }
            Err(why) => {
                tracing::info!("hardware decoding unavailable: {why}");
                Probe::Failed(why)
            }
        };
        self.lock().probe = probe;
        self.probed.notify_all();
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HwState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub(crate) fn status(&self) -> HardwareDecoding {
        match &self.lock().probe {
            Probe::Running => HardwareDecoding::Checking,
            Probe::Ready(gpu) => HardwareDecoding::Available {
                device: gpu.adapter.clone(),
                codecs: gpu.codecs.clone(),
            },
            Probe::Failed(why) => HardwareDecoding::Unavailable(why.clone()),
            // A new device is made by the next decoder; until then, say what we had.
            Probe::Lost => HardwareDecoding::Unavailable(
                "The graphics card was reset; checking again with the next stream".to_owned(),
            ),
        }
    }

    pub(crate) fn set_on(&self, on: bool) {
        self.lock().on = on;
    }

    /// The device for a new decoder of `codec`, if hardware decoding is on and the device
    /// decodes it. Waits for a running probe (up to [`PROBE_WAIT`]); probes again after a lost
    /// device.
    pub(crate) fn device(&self, codec: VideoCodec) -> Option<Arc<Gpu>> {
        let mut state = self.lock();
        if !state.on {
            return None;
        }
        let until = Instant::now() + PROBE_WAIT;
        while matches!(state.probe, Probe::Running) {
            let left = until.saturating_duration_since(Instant::now());
            if left.is_zero() {
                tracing::info!("the graphics card check is still running; decoding in software");
                return None;
            }
            state = self
                .probed
                .wait_timeout(state, left)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
        if matches!(state.probe, Probe::Lost) {
            // Probe again here, on this decode thread; other callers wait for it (rare, and a
            // few hundred milliseconds).
            state.probe = match Gpu::probe() {
                Ok(gpu) => {
                    tracing::info!(adapter = %gpu.adapter, "graphics device created again");
                    Probe::Ready(Arc::new(gpu))
                }
                Err(why) => {
                    tracing::warn!("hardware decoding unavailable after a device loss: {why}");
                    Probe::Failed(why)
                }
            };
        }
        match &state.probe {
            Probe::Ready(gpu) if gpu.codecs.contains(&codec) => Some(gpu.clone()),
            _ => None,
        }
    }

    /// `gpu` was lost: forget it (unless a new one already replaced it).
    pub(crate) fn lost(&self, gpu: &Arc<Gpu>) {
        let mut state = self.lock();
        if let Probe::Ready(current) = &state.probe
            && Arc::ptr_eq(current, gpu)
        {
            tracing::warn!(adapter = %gpu.adapter, "the graphics device was lost");
            state.probe = Probe::Lost;
        }
    }
}

/// Gets one decoder's pictures out of GPU memory, shrinking them first when asked.
pub(crate) struct CopyBack {
    gpu: Arc<Gpu>,
    /// CPU-readable copy of the last picture's size.
    staging: Option<(ID3D11Texture2D, (u32, u32))>,
    scaler: Option<GpuScaler>,
    /// To wait for the copy without a blocking Map (`None`: the driver has no fences).
    fence: Option<Fence>,
}

/// A fence and an event to wait for the GPU's work on this decoder's copies.
struct Fence {
    fence: ID3D11Fence,
    event: HANDLE,
    value: u64,
}

impl Fence {
    fn new(gpu: &Gpu) -> windows_core::Result<Self> {
        // SAFETY: plain creation calls; the event handle is owned and closed in Drop.
        unsafe {
            let device: ID3D11Device5 = gpu.device.cast()?;
            let mut fence: Option<ID3D11Fence> = None;
            device.CreateFence(0, D3D11_FENCE_FLAG_NONE, &mut fence)?;
            let fence = fence.ok_or_else(|| windows_core::Error::from(E_FAIL))?;
            let event = CreateEventW(None, false, false, None)?;
            Ok(Self {
                fence,
                event,
                value: 0,
            })
        }
    }

    /// Queues a signal after the work queued so far, and sends the work to the GPU.
    fn signal(&mut self, context: &ID3D11DeviceContext) -> windows_core::Result<()> {
        self.value += 1;
        // SAFETY: the fence belongs to the context's device; called with the device locked.
        unsafe {
            let context: ID3D11DeviceContext4 = context.cast()?;
            context.Signal(&self.fence, self.value)?;
            context.Flush();
        }
        Ok(())
    }

    /// Waits (at most a second) until the GPU passed the last signal.
    fn wait(&self) {
        // SAFETY: the fence and event are valid; waiting on our own event.
        unsafe {
            if self.fence.GetCompletedValue() >= self.value {
                return;
            }
            if self
                .fence
                .SetEventOnCompletion(self.value, self.event)
                .is_ok()
            {
                WaitForSingleObject(self.event, 1000);
            }
        }
    }
}

impl Drop for Fence {
    fn drop(&mut self) {
        // SAFETY: our own event handle, closed once.
        unsafe {
            let _ = CloseHandle(self.event);
        }
    }
}

impl std::fmt::Debug for CopyBack {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CopyBack")
            .field("gpu", &self.gpu)
            .finish_non_exhaustive()
    }
}

/// Records the time since its start as one copy-back when dropped.
struct TimerFrom(Instant);

impl Drop for TimerFrom {
    fn drop(&mut self) {
        profile::record(Stage::CopyBack, self.0.elapsed());
    }
}

/// The Video Processor set up for one input and output size.
struct GpuScaler {
    /// Input (visible) size and output size.
    key: ((u32, u32), (u32, u32)),
    enumerator: ID3D11VideoProcessorEnumerator,
    processor: ID3D11VideoProcessor,
    output: ID3D11Texture2D,
    output_view: ID3D11VideoProcessorOutputView,
    /// One view per decoder surface (texture, array slice), and the blit that last used it.
    /// The decoder reuses a small pool.
    inputs: HashMap<(usize, u32), (ID3D11VideoProcessorInputView, u64)>,
    blits: u64,
}

/// More views than any decoder's surface pool: the pool was replaced, start over.
const MAX_INPUT_VIEWS: usize = 64;
/// Every this many blits, views not used since the last sweep are dropped. Each holds its
/// surface, so a pool the decoder replaced would otherwise stay in GPU memory.
const INPUT_VIEW_SWEEP: u64 = 120;

/// BT.709, studio range (16-235) for both sides, so the Video Processor only scales.
fn color_space() -> D3D11_VIDEO_PROCESSOR_COLOR_SPACE {
    // Bitfield: Usage (bit 0), RGB_Range (1), YCbCr_Matrix (2, 1 = BT.709), YCbCr_xvYCC (3),
    // Nominal_Range (4-5, 1 = 16-235).
    D3D11_VIDEO_PROCESSOR_COLOR_SPACE {
        _bitfield: (1 << 2) | (1 << 4),
    }
}

impl CopyBack {
    pub(crate) fn new(gpu: Arc<Gpu>) -> Self {
        let fence = Fence::new(&gpu)
            .map_err(|e| tracing::debug!(error = %e, "no D3D11 fence; waiting in Map instead"))
            .ok();
        Self {
            gpu,
            staging: None,
            scaler: None,
            fence,
        }
    }

    pub(crate) fn gpu(&self) -> &Arc<Gpu> {
        &self.gpu
    }

    /// Whether this decoder output texture can be copied here (NV12 only).
    pub(crate) fn handles(texture: &ID3D11Texture2D) -> bool {
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        // SAFETY: a plain query on a valid texture.
        unsafe { texture.GetDesc(&mut desc) };
        desc.Format == DXGI_FORMAT_NV12
    }

    /// Copies the `visible` part (x, y, width, height) of array slice `index` of `texture`
    /// into a [`Frame`], shrunk on the GPU to [`limited_size`] under `limit`.
    pub(crate) fn copy(
        &mut self,
        texture: &ID3D11Texture2D,
        index: u32,
        visible: (u32, u32, u32, u32),
        limit: Option<(u32, u32)>,
    ) -> windows_core::Result<Option<Frame>> {
        let (vx, vy, vw, vh) = (
            visible.0 & !1,
            visible.1 & !1,
            visible.2 & !1,
            visible.3 & !1,
        );
        if vw == 0 || vh == 0 {
            return Ok(None);
        }
        let out = limited_size(vw, vh, limit);
        let gpu = self.gpu.clone();
        // SAFETY: every resource belongs to this device; the mapped pointer is valid for
        // RowPitch * height * 3 / 2 bytes (NV12) until Unmap, and the slices built from it
        // don't outlive the mapping.
        unsafe {
            let staging = self.staging(out)?;
            let copy_started;
            {
                let _lock = gpu.lock();
                if out == (vw, vh) {
                    // Not scaling: drop the scaler, whose input views hold decoder surfaces
                    // (only blits sweep them).
                    self.scaler = None;
                    copy_started = Instant::now();
                    let region = D3D11_BOX {
                        left: vx,
                        top: vy,
                        front: 0,
                        right: vx + vw,
                        bottom: vy + vh,
                        back: 1,
                    };
                    gpu.context.CopySubresourceRegion(
                        &staging,
                        0,
                        0,
                        0,
                        0,
                        texture,
                        index,
                        Some(&region),
                    );
                } else {
                    let started = Instant::now();
                    let scaler = self.scaler((vw, vh), out)?;
                    scaler.blit(&gpu, texture, index, (vx, vy, vw, vh))?;
                    profile::record(Stage::GpuScale, started.elapsed());
                    copy_started = Instant::now();
                    gpu.context.CopyResource(&staging, &scaler.output);
                }
                if let Some(fence) = &mut self.fence {
                    fence.signal(&gpu.context)?;
                }
            }
            let _t = TimerFrom(copy_started);
            // Wait for the GPU without holding the device (other streams keep going) and
            // without spinning, which a blocking Map does on some drivers.
            if let Some(fence) = &self.fence {
                fence.wait();
            }
            let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
            {
                let _lock = gpu.lock();
                gpu.context
                    .Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))?;
            }
            // Copied out without holding the device, so other streams' decoding goes on
            // meanwhile; the staging texture is this decoder's own.
            let pitch = mapped.RowPitch as usize;
            let (w, h) = (out.0 as usize, out.1 as usize);
            let data = std::slice::from_raw_parts(mapped.pData.cast::<u8>(), pitch * h * 3 / 2);
            let frame = Yuv420Planes {
                y: data,
                y_stride: pitch,
                chroma: Chroma::Interleaved {
                    uv: &data[pitch * h..],
                    stride: pitch,
                },
                width: w as u32,
                height: h as u32,
            }
            .to_frame()
            .map(|mut f| {
                f.source_size = (vw, vh);
                f
            });
            let _lock = gpu.lock();
            gpu.context.Unmap(&staging, 0);
            Ok(frame)
        }
    }

    /// The staging texture for `size`, made when the size changes.
    fn staging(&mut self, size: (u32, u32)) -> windows_core::Result<ID3D11Texture2D> {
        if let Some((t, s)) = &self.staging
            && *s == size
        {
            return Ok(t.clone());
        }
        let texture = self.gpu.texture(size.0, size.1, true)?;
        self.staging = Some((texture.clone(), size));
        Ok(texture)
    }

    fn scaler(
        &mut self,
        input: (u32, u32),
        output: (u32, u32),
    ) -> windows_core::Result<&mut GpuScaler> {
        if self
            .scaler
            .as_ref()
            .is_none_or(|s| s.key != (input, output))
        {
            self.scaler = None;
            self.scaler = Some(GpuScaler::new(&self.gpu, input, output)?);
            tracing::debug!(?input, ?output, "GPU scaler ready");
        }
        Ok(self.scaler.as_mut().expect("scaler was just set"))
    }
}

impl GpuScaler {
    fn new(gpu: &Gpu, input: (u32, u32), output: (u32, u32)) -> windows_core::Result<Self> {
        let rate = DXGI_RATIONAL {
            Numerator: 30,
            Denominator: 1,
        };
        let content = D3D11_VIDEO_PROCESSOR_CONTENT_DESC {
            InputFrameFormat: D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
            InputFrameRate: rate,
            InputWidth: input.0,
            InputHeight: input.1,
            OutputFrameRate: rate,
            OutputWidth: output.0,
            OutputHeight: output.1,
            Usage: D3D11_VIDEO_USAGE_OPTIMAL_SPEED,
        };
        // SAFETY: valid descriptions; the created objects are owned references.
        unsafe {
            let enumerator = gpu.video_device.CreateVideoProcessorEnumerator(&content)?;
            let processor = gpu.video_device.CreateVideoProcessor(&enumerator, 0)?;
            let texture = gpu.texture(output.0, output.1, false)?;
            let view_desc = D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC {
                ViewDimension: D3D11_VPOV_DIMENSION_TEXTURE2D,
                Anonymous: D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC_0 {
                    Texture2D: D3D11_TEX2D_VPOV { MipSlice: 0 },
                },
            };
            let mut output_view = None;
            gpu.video_device.CreateVideoProcessorOutputView(
                &texture,
                &enumerator,
                &view_desc,
                Some(&mut output_view),
            )?;
            let output_view = output_view.ok_or_else(|| windows_core::Error::from(E_FAIL))?;

            let vc = &gpu.video_context;
            let full = RECT {
                left: 0,
                top: 0,
                right: output.0 as i32,
                bottom: output.1 as i32,
            };
            vc.VideoProcessorSetStreamFrameFormat(
                &processor,
                0,
                D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
            );
            vc.VideoProcessorSetStreamAutoProcessingMode(&processor, 0, false);
            vc.VideoProcessorSetStreamColorSpace(&processor, 0, &color_space());
            vc.VideoProcessorSetOutputColorSpace(&processor, &color_space());
            vc.VideoProcessorSetStreamDestRect(&processor, 0, true, Some(&full));
            vc.VideoProcessorSetOutputTargetRect(&processor, true, Some(&full));

            Ok(Self {
                key: (input, output),
                enumerator,
                processor,
                output: texture,
                output_view,
                inputs: HashMap::new(),
                blits: 0,
            })
        }
    }

    /// Scales the `visible` part of the decoder surface into `self.output`.
    fn blit(
        &mut self,
        gpu: &Gpu,
        texture: &ID3D11Texture2D,
        index: u32,
        visible: (u32, u32, u32, u32),
    ) -> windows_core::Result<()> {
        let key = (texture.as_raw() as usize, index);
        self.blits += 1;
        let now = self.blits;
        if now.is_multiple_of(INPUT_VIEW_SWEEP) {
            self.inputs
                .retain(|_, (_, used)| now - *used < INPUT_VIEW_SWEEP);
        }
        if !self.inputs.contains_key(&key) {
            if self.inputs.len() >= MAX_INPUT_VIEWS {
                self.inputs.clear();
            }
            let desc = D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC {
                FourCC: 0,
                ViewDimension: D3D11_VPIV_DIMENSION_TEXTURE2D,
                Anonymous: D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC_0 {
                    Texture2D: D3D11_TEX2D_VPIV {
                        MipSlice: 0,
                        ArraySlice: index,
                    },
                },
            };
            let mut view = None;
            // SAFETY: the texture and enumerator belong to this device.
            unsafe {
                gpu.video_device.CreateVideoProcessorInputView(
                    texture,
                    &self.enumerator,
                    &desc,
                    Some(&mut view),
                )?;
            }
            let view = view.ok_or_else(|| windows_core::Error::from(E_FAIL))?;
            self.inputs.insert(key, (view, now));
        }
        let view = self.inputs.get_mut(&key).map(|(view, used)| {
            *used = now;
            view.clone()
        });
        let (x, y, w, h) = visible;
        let source = RECT {
            left: x as i32,
            top: y as i32,
            right: (x + w) as i32,
            bottom: (y + h) as i32,
        };
        let stream = D3D11_VIDEO_PROCESSOR_STREAM {
            Enable: true.into(),
            pInputSurface: std::mem::ManuallyDrop::new(view),
            ..Default::default()
        };
        // SAFETY: valid processor, views and stream description; the stream's view reference
        // is released right after the call.
        unsafe {
            gpu.video_context.VideoProcessorSetStreamSourceRect(
                &self.processor,
                0,
                true,
                Some(&source),
            );
            let result = gpu.video_context.VideoProcessorBlt(
                &self.processor,
                &self.output_view,
                0,
                std::slice::from_ref(&stream),
            );
            drop(std::mem::ManuallyDrop::into_inner(stream.pInputSurface));
            result
        }
    }
}
