//! After shutdown and release, nothing of the DLL stays alive (objects, helper threads), so
//! `DllCanUnloadNow` can succeed. A separate test binary, so no other test holds objects.
#![cfg(windows)]

use std::time::{Duration, Instant};

use rtspcam_vcam::guard::live_count;
use rtspcam_vcam::{DllCanUnloadNow, RTSPCAM_ATTR_CAMERA_ID, create_activate};
use windows::Win32::Foundation::S_OK;
use windows::Win32::Media::MediaFoundation::{
    IMFMediaSource, MF_SOURCE_READER_FIRST_VIDEO_STREAM, MF_VERSION,
    MFCreateSourceReaderFromMediaSource, MFSTARTUP_NOSOCKET, MFStartup,
};
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx};
use windows_core::HSTRING;

#[test]
fn shutdown_releases_everything() {
    // SAFETY: COM/MF initialization and plain calls on valid objects.
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        MFStartup(MF_VERSION, MFSTARTUP_NOSOCKET).unwrap();
        for _ in 0..3 {
            let activate = create_activate().unwrap();
            activate
                .SetString(
                    &RTSPCAM_ATTR_CAMERA_ID,
                    &HSTRING::from(uuid::Uuid::new_v4().to_string()),
                )
                .unwrap();
            let source: IMFMediaSource = activate.ActivateObject().unwrap();
            let reader = MFCreateSourceReaderFromMediaSource(&source, None).unwrap();
            let stream = MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32;
            for _ in 0..3 {
                let (mut flags, mut sample) = (0u32, None);
                // The flags out-param is mandatory in synchronous mode.
                reader
                    .ReadSample(stream, 0, None, Some(&mut flags), None, Some(&mut sample))
                    .unwrap();
            }
            activate.ShutdownObject().unwrap();
            drop(reader);
            drop(source);
            drop(activate);
        }
    }
    // Helper threads exit asynchronously; the feed thread may be in its 1 s retry wait.
    let deadline = Instant::now() + Duration::from_secs(3);
    while live_count() > 0 && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(live_count(), 0, "objects or threads still alive");
    assert_eq!(DllCanUnloadNow(), S_OK);
}
