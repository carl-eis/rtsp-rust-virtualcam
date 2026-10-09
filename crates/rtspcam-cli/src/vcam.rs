//! `vcam`: register the media source DLL, create virtual cameras fed by a test pattern or an
//! RTSP stream, list cameras, and exercise the DLL in-process.

use std::ffi::c_void;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context as _, bail};
use clap::{Args, Subcommand};
use rtspcam_core::FitMode;
use rtspcam_core::config::ConfigStore;
use rtspcam_core::constants::vcam_source_clsid_string;
use rtspcam_ipc::server::{FrameSource, serve};
use rtspcam_ipc::{PixelFormat, StreamStatus, VideoFormat};
use rtspcam_pipeline::pattern::{read_counter, test_pattern};
use rtspcam_pipeline::scale::nv12_to_bgra;
use rtspcam_pipeline::{
    Frame, FrameBus, Matrix, Pipeline, PipelineOptions, Scaler, SourceOptions, StreamState,
};
use rtspcam_vcam_mgr::{MfThread, VirtualCamera, list_devices};
use uuid::Uuid;
use windows::Win32::Media::MediaFoundation::{
    IMFActivate, IMFMediaSource, MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE,
    MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_GUID,
    MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_SYMBOLIC_LINK, MF_MT_FRAME_RATE, MF_MT_FRAME_SIZE,
    MF_MT_SUBTYPE, MF_SOURCE_READER_FIRST_VIDEO_STREAM, MFCreateAttributes, MFCreateDeviceSource,
    MFCreateSourceReaderFromMediaSource, MFVideoFormat_NV12,
};
use windows::Win32::System::Com::IClassFactory;
use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
use windows::Win32::System::Registry::{HKEY_LOCAL_MACHINE, RRF_RT_REG_SZ, RegGetValueW};
use windows_core::{GUID, HRESULT, HSTRING, Interface as _, PCWSTR};

#[derive(Debug, Subcommand)]
pub(crate) enum VcamCommand {
    /// Register the media source DLL (HKLM; run as administrator).
    Register(DllArg),
    /// Unregister the media source DLL (run as administrator).
    Unregister(DllArg),
    /// Show whether virtual cameras can work on this machine.
    Status,
    /// List video capture devices; RTSP Cam cameras show their stream id.
    List,
    /// Create a virtual camera fed by a test pattern or an RTSP stream, until Ctrl+C.
    Add(AddArgs),
    /// Load the DLL in this process and read frames from it like a camera app would.
    Test(TestArgs),
    /// Open a camera through Windows (Frame Server) like a camera app and read frames.
    Read(ReadArgs),
}

#[derive(Debug, Args)]
pub(crate) struct DllArg {
    /// Path of rtspcam_vcam.dll (default: next to this program).
    #[arg(long)]
    dll: Option<PathBuf>,
}

#[derive(Debug, Args)]
pub(crate) struct AddArgs {
    /// Camera name shown in apps.
    #[arg(long)]
    name: String,
    /// Feed it an animated test pattern.
    #[arg(long, conflicts_with = "stream")]
    pattern: bool,
    /// Feed it this RTSP URL or config stream name.
    #[arg(long, required_unless_present = "pattern")]
    stream: Option<String>,
    /// Stop after this many seconds (default: until Ctrl+C).
    #[arg(long)]
    seconds: Option<u64>,
}

#[derive(Debug, Args)]
pub(crate) struct ReadArgs {
    /// Camera name (case-insensitive substring), as `vcam list` shows it.
    name: String,
    /// Seconds to read for.
    #[arg(long, default_value_t = 5)]
    seconds: u64,
    /// Count distinct frames of the test pattern (camera made with `vcam add --pattern`).
    #[arg(long)]
    pattern: bool,
    /// Save the last sample as BMP (NV12 1280x720 only).
    #[arg(long, value_name = "FILE")]
    snapshot: Option<PathBuf>,
}

#[derive(Debug, Args)]
pub(crate) struct TestArgs {
    #[command(flatten)]
    dll: DllArg,
    /// Seconds to read for.
    #[arg(long, default_value_t = 3)]
    seconds: u64,
    /// Serve the test pattern to the in-process camera (otherwise it shows its placeholder).
    #[arg(long)]
    pattern: bool,
    /// Save the last sample as BMP (NV12 formats only).
    #[arg(long, value_name = "FILE")]
    snapshot: Option<PathBuf>,
}

pub(crate) fn run(store: &ConfigStore, cmd: VcamCommand) -> anyhow::Result<ExitCode> {
    match cmd {
        VcamCommand::Register(a) => call_export(&dll_path(a.dll)?, "DllRegisterServer"),
        VcamCommand::Unregister(a) => call_export(&dll_path(a.dll)?, "DllUnregisterServer"),
        VcamCommand::Status => status(),
        VcamCommand::List => list(),
        VcamCommand::Add(a) => add(store, a),
        VcamCommand::Test(a) => test(a),
        VcamCommand::Read(a) => read(a),
    }
}

fn dll_path(arg: Option<PathBuf>) -> anyhow::Result<PathBuf> {
    let path = match arg {
        Some(p) => p,
        None => std::env::current_exe()?.with_file_name("rtspcam_vcam.dll"),
    };
    let path = std::path::absolute(&path)?;
    if !path.exists() {
        bail!(
            "{} not found (build it with `cargo build -p rtspcam-vcam`)",
            path.display()
        );
    }
    Ok(path)
}

/// Loads the DLL and returns the address of an export.
fn export(dll: &Path, name: &str) -> anyhow::Result<*const c_void> {
    let path = HSTRING::from(dll.as_os_str());
    let name = std::ffi::CString::new(name)?;
    // SAFETY: loading a DLL by full path (kept loaded for the process lifetime) and looking
    // up an export by a null-terminated name.
    unsafe {
        let module = LoadLibraryW(PCWSTR(path.as_ptr()))
            .with_context(|| format!("could not load {}", dll.display()))?;
        GetProcAddress(module, windows_core::PCSTR(name.as_ptr().cast()))
            .map(|f| f as *const c_void)
            .with_context(|| format!("{} has no export {name:?}", dll.display()))
    }
}

fn call_export(dll: &Path, name: &str) -> anyhow::Result<ExitCode> {
    let f = export(dll, name)?;
    // SAFETY: DllRegisterServer / DllUnregisterServer take no arguments and return HRESULT.
    let hr = unsafe { std::mem::transmute::<*const c_void, extern "system" fn() -> HRESULT>(f)() };
    if hr.is_ok() {
        println!("{name}: OK ({})", dll.display());
        if name == "DllRegisterServer" && !in_program_files(dll) {
            println!(
                "warning: the Frame Server service may not be able to read this folder. \
                 Copy the DLL to C:\\Program Files\\RtspCam\\ and register it from there."
            );
        }
        Ok(ExitCode::SUCCESS)
    } else {
        println!("{name} failed: {}", windows_core::Error::from(hr));
        if hr == HRESULT(0x8007_0005_u32 as i32) {
            println!("hint:  run this command from an administrator prompt");
        }
        Ok(ExitCode::FAILURE)
    }
}

fn in_program_files(path: &Path) -> bool {
    std::env::var_os("ProgramFiles").is_some_and(|pf| path.starts_with(pf))
}

/// The DLL path registered for our CLSID, if any.
fn registered_dll() -> Option<PathBuf> {
    let key = HSTRING::from(format!(
        r"Software\Classes\CLSID\{}\InprocServer32",
        vcam_source_clsid_string()
    ));
    let mut buf = vec![0u16; 1024];
    let mut size = (buf.len() * 2) as u32;
    // SAFETY: valid key path and buffer of `size` bytes.
    let err = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            PCWSTR(key.as_ptr()),
            PCWSTR::null(),
            RRF_RT_REG_SZ,
            None,
            Some(buf.as_mut_ptr().cast()),
            Some(&mut size),
        )
    };
    if err.is_err() {
        return None;
    }
    let len = (size as usize / 2).saturating_sub(1);
    Some(PathBuf::from(String::from_utf16_lossy(&buf[..len])))
}

fn status() -> anyhow::Result<ExitCode> {
    let mut ok = true;
    if rtspcam_vcam_mgr::is_supported() {
        println!("virtual cameras: supported (MFCreateVirtualCamera found)");
    } else {
        println!("virtual cameras: NOT supported (Windows 11 required)");
        ok = false;
    }
    match registered_dll() {
        Some(path) => {
            let exists = path.exists();
            println!(
                "media source:    registered -> {}{}",
                path.display(),
                if exists { "" } else { "  (FILE MISSING)" }
            );
            if !in_program_files(&path) {
                println!(
                    "                 warning: not under Program Files; Frame Server may be denied access"
                );
            }
            ok &= exists;
        }
        None => {
            println!(
                "media source:    NOT registered (run `rtspcam-cli vcam register` as administrator)"
            );
            ok = false;
        }
    }
    Ok(if ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}

fn list() -> anyhow::Result<ExitCode> {
    let _mf = MfThread::init()?;
    let devices = list_devices()?;
    if devices.is_empty() {
        println!("no video capture devices");
    }
    for d in devices {
        match d.rtspcam_id {
            Some(id) => println!("{}  [RTSP Cam stream {id}]", d.name),
            None => println!("{}", d.name),
        }
        println!("    {}", d.symbolic_link);
    }
    Ok(ExitCode::SUCCESS)
}

/// Serves the newest frame of a bus, scaled and converted to whatever each client asks for.
pub(crate) struct BusSource {
    bus: FrameBus,
    fit: FitMode,
    status: Box<dyn Fn() -> (StreamStatus, String) + Send + Sync>,
    state: Mutex<BusState>,
}

#[derive(Default)]
struct BusState {
    scaler: Scaler,
    last: usize,
    seq: u64,
}

impl BusSource {
    pub(crate) fn new(
        bus: FrameBus,
        fit: FitMode,
        status: impl Fn() -> (StreamStatus, String) + Send + Sync + 'static,
    ) -> Self {
        Self {
            bus,
            fit,
            status: Box::new(status),
            state: Mutex::default(),
        }
    }
}

impl FrameSource for BusSource {
    fn client_connected(&self, format: VideoFormat) {
        println!("camera in use: {format}");
    }

    fn client_disconnected(&self) {
        println!("camera released");
    }

    fn next_frame(
        &self,
        format: VideoFormat,
        after: Option<u64>,
        out: &mut Vec<u8>,
    ) -> Option<u64> {
        let frame = self.bus.latest()?;
        let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let id = Arc::as_ptr(&frame) as usize;
        if id != st.last {
            st.last = id;
            st.seq += 1;
        }
        if after == Some(st.seq) {
            return None;
        }
        let scaler = &mut st.scaler;
        match format.pixel_format {
            PixelFormat::Nv12 => {
                scaler.scale_into(&frame, format.width, format.height, self.fit, out)
            }
            PixelFormat::Rgb32 => {
                let scaled = scaler.scale(&frame, format.width, format.height, self.fit);
                nv12_to_bgra(&scaled, Matrix::for_height(frame.height()), out);
            }
        }
        Some(st.seq)
    }

    fn status(&self) -> (StreamStatus, String) {
        (self.status)()
    }
}

fn ipc_status(state: &StreamState) -> (StreamStatus, String) {
    match state {
        StreamState::Streaming(_) => (StreamStatus::Streaming, String::new()),
        StreamState::Idle | StreamState::Connecting { .. } => {
            (StreamStatus::Connecting, String::new())
        }
        StreamState::Retrying { error, .. } => (StreamStatus::Error, error.to_string()),
    }
}

/// Publishes the test pattern to a bus at 30 fps until the task is dropped.
async fn run_pattern(bus: FrameBus) {
    let mut tick = tokio::time::interval(Duration::from_millis(1000 / 30));
    let mut n = 0u64;
    loop {
        tick.tick().await;
        n += 1;
        let frame = tokio::task::spawn_blocking(move || test_pattern(1280, 720, n))
            .await
            .expect("pattern task");
        bus.publish(frame);
    }
}

fn add(store: &ConfigStore, args: AddArgs) -> anyhow::Result<ExitCode> {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    let _mf = MfThread::init()?;

    let (id, source, pipeline, preferred): (Uuid, Arc<dyn FrameSource>, Option<Pipeline>, _) =
        if args.pattern {
            let bus = FrameBus::new();
            rt.spawn(run_pattern(bus.clone()));
            let source = BusSource::new(bus, FitMode::Stretch, || {
                (StreamStatus::Streaming, String::new())
            });
            (Uuid::new_v4(), Arc::new(source), None, None)
        } else {
            let target = args
                .stream
                .as_deref()
                .expect("clap requires --stream without --pattern");
            let (id, opts, preferred, fit) = if target.contains("://") {
                (
                    Uuid::new_v4(),
                    SourceOptions::from_url(target)?,
                    None,
                    FitMode::Letterbox,
                )
            } else {
                let config = store.load()?;
                let s = config
                    .streams
                    .iter()
                    .find(|s| s.name.trim().eq_ignore_ascii_case(target.trim()))
                    .with_context(|| format!("no stream named \"{target}\""))?;
                let o = s.output;
                (
                    s.id,
                    SourceOptions::from_stream(s)?,
                    Some((o.width, o.height, o.fps)),
                    s.fit_mode,
                )
            };
            let pipeline = Pipeline::start(target, PipelineOptions::new(opts), rt.handle());
            let status = pipeline.status();
            let source = BusSource::new(pipeline.frames().clone(), fit, move || {
                ipc_status(&status.borrow())
            });
            (id, Arc::new(source), Some(pipeline), preferred)
        };

    let listener = {
        let _enter = rt.enter();
        rtspcam_platform::frame_transport().listen(id)?
    };
    let server = rt.spawn(serve(listener, source));
    let camera = VirtualCamera::create(&args.name, id, preferred)?;
    println!(
        "camera \"{}\" created (stream id {id}). Open it in the Camera app, OBS or Discord.",
        args.name
    );
    println!("Press Ctrl+C to remove it.");

    rt.block_on(async {
        let ctrl_c = tokio::signal::ctrl_c();
        match args.seconds {
            Some(s) => {
                tokio::select! {
                    _ = ctrl_c => {}
                    _ = tokio::time::sleep(Duration::from_secs(s)) => {}
                }
            }
            None => {
                let _ = ctrl_c.await;
            }
        }
    });
    drop(camera);
    server.abort();
    if let Some(p) = pipeline {
        p.stop();
    }
    println!("camera removed");
    Ok(ExitCode::SUCCESS)
}

fn test(args: TestArgs) -> anyhow::Result<ExitCode> {
    let dll = dll_path(args.dll.dll)?;
    let get_class_object = export(&dll, "DllGetClassObject")?;
    let _mf = MfThread::init()?;
    let camera = Uuid::new_v4();

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    if args.pattern {
        let bus = FrameBus::new();
        rt.spawn(run_pattern(bus.clone()));
        let source = BusSource::new(bus, FitMode::Stretch, || {
            (StreamStatus::Streaming, String::new())
        });
        let listener = {
            let _enter = rt.enter();
            rtspcam_platform::frame_transport().listen(camera)?
        };
        rt.spawn(serve(listener, Arc::new(source)));
    }

    type GetClassObject =
        unsafe extern "system" fn(*const GUID, *const GUID, *mut *mut c_void) -> HRESULT;
    let clsid = GUID::from_u128(rtspcam_core::constants::VCAM_SOURCE_CLSID.as_u128());
    // SAFETY: DllGetClassObject's documented signature; the returned pointers are owned COM
    // references; the reader and buffers are used per their contracts.
    unsafe {
        let f = std::mem::transmute::<*const c_void, GetClassObject>(get_class_object);
        let mut raw = std::ptr::null_mut();
        f(&clsid, &IClassFactory::IID, &mut raw).ok()?;
        let factory = IClassFactory::from_raw(raw);
        let activate: IMFActivate = factory.CreateInstance(None)?;
        activate.SetString(
            &rtspcam_vcam_attr_camera_id(),
            &HSTRING::from(camera.to_string()),
        )?;
        let source: IMFMediaSource = activate.ActivateObject()?;
        let result = consume(
            &source,
            args.seconds,
            args.pattern,
            args.snapshot.as_deref(),
        );
        source.Shutdown()?;
        result?;
    }
    Ok(ExitCode::SUCCESS)
}

/// Reads from a media source like a camera app: prints its formats, reads for `seconds`,
/// reports the frame rate (and distinct pattern frames), optionally saves the last frame.
fn consume(
    source: &IMFMediaSource,
    seconds: u64,
    pattern: bool,
    snapshot: Option<&Path>,
) -> anyhow::Result<()> {
    // SAFETY: the reader and its samples and buffers are used per their contracts; the locked
    // buffer is copied before Unlock.
    unsafe {
        let reader = MFCreateSourceReaderFromMediaSource(source, None)?;
        let stream = MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32;

        println!("formats offered:");
        for i in 0.. {
            let Ok(t) = reader.GetNativeMediaType(stream, i) else {
                break;
            };
            let size = t.GetUINT64(&MF_MT_FRAME_SIZE)?;
            let rate = t.GetUINT64(&MF_MT_FRAME_RATE)?;
            let nv12 = t.GetGUID(&MF_MT_SUBTYPE)? == MFVideoFormat_NV12;
            println!(
                "  {:>2}: {} {}x{} @ {}",
                i,
                if nv12 { "NV12 " } else { "RGB32" },
                size >> 32,
                size as u32,
                (rate >> 32) / (rate & 0xffff_ffff).max(1)
            );
        }

        let start = Instant::now();
        let mut samples = 0u32;
        let mut last: Option<Vec<u8>> = None;
        let mut counters = Vec::new();
        while start.elapsed() < Duration::from_secs(seconds) {
            let (mut flags, mut sample) = (0u32, None);
            reader.ReadSample(stream, 0, None, Some(&mut flags), None, Some(&mut sample))?;
            let Some(sample) = sample else { continue };
            samples += 1;
            let buffer = sample.ConvertToContiguousBuffer()?;
            let (mut ptr, mut len) = (std::ptr::null_mut(), 0u32);
            buffer.Lock(&mut ptr, None, Some(&mut len))?;
            let data = std::slice::from_raw_parts(ptr, len as usize).to_vec();
            buffer.Unlock()?;
            if pattern && data.len() == 1280 * 720 * 3 / 2 {
                counters.push(read_counter(&Frame::from_nv12(1280, 720, data.clone())));
            }
            last = Some(data);
        }
        let secs = start.elapsed().as_secs_f64();
        println!(
            "read {samples} samples in {secs:.2} s = {:.1} fps",
            f64::from(samples) / secs
        );
        if pattern {
            counters.dedup();
            println!("{} distinct pattern frames received", counters.len());
        }
        if let (Some(path), Some(data)) = (snapshot, last)
            && data.len() == 1280 * 720 * 3 / 2
        {
            crate::rtsp::save_bmp(path, &Frame::from_nv12(1280, 720, data))?;
            println!("snapshot: {}", path.display());
        }
    }
    Ok(())
}

/// `rtspcam_vcam::RTSPCAM_ATTR_CAMERA_ID` (duplicated so the CLI doesn't link the DLL crate).
fn rtspcam_vcam_attr_camera_id() -> GUID {
    GUID::from_u128(0xc5464fce_84dc_4420_8bc5_53830a558a46)
}

fn read(args: ReadArgs) -> anyhow::Result<ExitCode> {
    let _mf = MfThread::init()?;
    let wanted = args.name.to_lowercase();
    let device = list_devices()?
        .into_iter()
        .find(|d| d.name.to_lowercase().contains(&wanted))
        .with_context(|| format!("no camera matching \"{}\" (see `vcam list`)", args.name))?;
    println!("opening {} ({})", device.name, device.symbolic_link);
    // SAFETY: attribute store and device source creation per their contracts.
    unsafe {
        let mut attrs = None;
        MFCreateAttributes(&mut attrs, 2)?;
        let attrs = attrs.context("MFCreateAttributes")?;
        attrs.SetGUID(
            &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE,
            &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_GUID,
        )?;
        attrs.SetString(
            &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_SYMBOLIC_LINK,
            &HSTRING::from(device.symbolic_link.as_str()),
        )?;
        let source = MFCreateDeviceSource(&attrs)?;
        let result = consume(
            &source,
            args.seconds,
            args.pattern,
            args.snapshot.as_deref(),
        );
        source.Shutdown()?;
        result?;
    }
    Ok(ExitCode::SUCCESS)
}
