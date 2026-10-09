//! The media source in-process, read through `IMFSourceReader` the way consumers do, with the
//! app side played by the real pipe server.
#![cfg(windows)]

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use rtspcam_ipc::server::{FrameSource, serve};
use rtspcam_ipc::{StreamStatus, VideoFormat};
use rtspcam_vcam::{RTSPCAM_ATTR_CAMERA_ID, create_activate};
use uuid::Uuid;
use windows::Win32::Media::MediaFoundation::{
    IMFMediaSource, IMFSourceReader, MF_MT_FRAME_RATE, MF_MT_FRAME_SIZE, MF_MT_SUBTYPE,
    MF_SOURCE_READER_FIRST_VIDEO_STREAM, MF_VERSION, MFCreateSourceReaderFromMediaSource,
    MFSTARTUP_NOSOCKET, MFStartup, MFVideoFormat_NV12, MFVideoFormat_RGB32,
};
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx};
use windows_core::HSTRING;

const STREAM: u32 = MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32;

fn init() {
    // SAFETY: plain COM/MF initialization for the test thread.
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        MFStartup(MF_VERSION, MFSTARTUP_NOSOCKET).unwrap();
    }
}

fn open(camera: Uuid) -> (IMFMediaSource, IMFSourceReader) {
    let activate = create_activate().unwrap();
    // SAFETY: valid objects; plain COM calls.
    unsafe {
        activate
            .SetString(&RTSPCAM_ATTR_CAMERA_ID, &HSTRING::from(camera.to_string()))
            .unwrap();
        let source: IMFMediaSource = activate.ActivateObject().unwrap();
        let reader = MFCreateSourceReaderFromMediaSource(&source, None).unwrap();
        (source, reader)
    }
}

/// Reads `n` samples; returns their data and timestamps.
fn read(reader: &IMFSourceReader, n: usize) -> Vec<(Vec<u8>, i64)> {
    let mut out = Vec::new();
    while out.len() < n {
        let mut flags = 0u32;
        let mut time = 0i64;
        let mut sample = None;
        // SAFETY: valid reader and out-params; the locked buffer is copied before Unlock.
        unsafe {
            reader
                .ReadSample(
                    STREAM,
                    0,
                    None,
                    Some(&mut flags),
                    Some(&mut time),
                    Some(&mut sample),
                )
                .unwrap();
            let Some(sample) = sample else { continue };
            let buffer = sample.ConvertToContiguousBuffer().unwrap();
            let mut ptr = std::ptr::null_mut();
            let mut len = 0u32;
            buffer.Lock(&mut ptr, None, Some(&mut len)).unwrap();
            out.push((std::slice::from_raw_parts(ptr, len as usize).to_vec(), time));
            buffer.Unlock().unwrap();
        }
    }
    out
}

fn set_format(reader: &IMFSourceReader, subtype: windows_core::GUID, w: u32, h: u32, fps: u32) {
    // SAFETY: valid reader; the native type list has this format.
    unsafe {
        for i in 0.. {
            let t = reader.GetNativeMediaType(STREAM, i).unwrap();
            if t.GetGUID(&MF_MT_SUBTYPE).unwrap() == subtype
                && t.GetUINT64(&MF_MT_FRAME_SIZE).unwrap() == (u64::from(w) << 32 | u64::from(h))
                && t.GetUINT64(&MF_MT_FRAME_RATE).unwrap() == (u64::from(fps) << 32 | 1)
            {
                reader.SetCurrentMediaType(STREAM, None, &t).unwrap();
                return;
            }
        }
    }
}

/// Frames whose every byte is the frame number (mod 256), and a fixed status.
#[derive(Default)]
struct Counter {
    seq: AtomicU64,
}

impl FrameSource for Counter {
    fn next_frame(&self, format: VideoFormat, _: Option<u64>, out: &mut Vec<u8>) -> Option<u64> {
        let seq = self.seq.fetch_add(1, Ordering::SeqCst) + 1;
        out.clear();
        out.resize(format.frame_len(), 200);
        Some(seq)
    }

    fn status(&self) -> (StreamStatus, String) {
        (StreamStatus::Streaming, String::new())
    }
}

#[test]
fn offers_twelve_formats_nv12_720p30_first() {
    init();
    let (source, reader) = open(Uuid::new_v4());
    // SAFETY: valid reader.
    unsafe {
        let mut count = 0;
        while reader.GetNativeMediaType(STREAM, count).is_ok() {
            count += 1;
        }
        assert_eq!(count, 12);
        let first = reader.GetNativeMediaType(STREAM, 0).unwrap();
        assert_eq!(first.GetGUID(&MF_MT_SUBTYPE).unwrap(), MFVideoFormat_NV12);
        assert_eq!(
            first.GetUINT64(&MF_MT_FRAME_SIZE).unwrap(),
            (1280u64 << 32) | 720
        );
        source.Shutdown().unwrap();
    }
}

#[test]
fn placeholder_at_a_steady_frame_rate_when_the_app_is_not_running() {
    init();
    let (source, reader) = open(Uuid::new_v4());
    read(&reader, 3); // warm up
    let start = Instant::now();
    let samples = read(&reader, 30);
    let elapsed = start.elapsed();
    // 30 frames at 30 fps: about one second, not instantly and not slower.
    assert!(
        elapsed > Duration::from_millis(850) && elapsed < Duration::from_millis(1300),
        "{elapsed:?}"
    );
    for (data, _) in &samples {
        assert_eq!(data.len(), 1280 * 720 * 3 / 2);
        assert!(data[0] < 50, "background should be dark: {}", data[0]);
        assert!(
            data.iter().take(1280 * 720).any(|&y| y > 200),
            "no text drawn"
        );
    }
    assert!(
        samples.windows(2).all(|w| w[0].1 < w[1].1),
        "timestamps must increase"
    );
    // SAFETY: valid source.
    unsafe { source.Shutdown().unwrap() };
}

#[test]
fn shows_app_frames_and_follows_format_changes() {
    init();
    let camera = Uuid::new_v4();
    let rt = tokio::runtime::Runtime::new().unwrap();
    let listener = {
        let _enter = rt.enter();
        rtspcam_platform::frame_transport().listen(camera).unwrap()
    };
    let server = rt.spawn(serve(listener, Arc::new(Counter::default())));
    std::thread::sleep(Duration::from_millis(100));

    let (source, reader) = open(camera);
    let wait_for_frames = |reader: &IMFSourceReader, len: usize| {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let (data, _) = read(reader, 1).pop().unwrap();
            assert_eq!(data.len(), len);
            if data.iter().all(|&b| b == 200) {
                return;
            }
            assert!(Instant::now() < deadline, "app frames never arrived");
        }
    };
    wait_for_frames(&reader, 1280 * 720 * 3 / 2);

    set_format(&reader, MFVideoFormat_RGB32, 640, 480, 15);
    wait_for_frames(&reader, 640 * 480 * 4);

    // SAFETY: valid source.
    unsafe { source.Shutdown().unwrap() };
    drop(reader);
    drop(source);
    server.abort();
}
