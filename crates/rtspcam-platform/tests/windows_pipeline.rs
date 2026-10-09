//! Full pipelines through Media Foundation against the `tools/test-rtsp` server. Runs only when
//! `RTSPCAM_TEST_SERVER` is set (for example `127.0.0.1:8554`).
#![cfg(windows)]

use std::time::{Duration, Instant};

use rtspcam_pipeline::decode::DecoderChoice;
use rtspcam_pipeline::{Pipeline, PipelineOptions, SourceOptions, StreamState};
use tokio::runtime::Runtime;

fn server() -> Option<String> {
    match std::env::var("RTSPCAM_TEST_SERVER") {
        Ok(s) if !s.trim().is_empty() => Some(s.trim().to_owned()),
        _ => {
            eprintln!("RTSPCAM_TEST_SERVER not set; skipping");
            None
        }
    }
}

#[test]
fn streams_h264_through_media_foundation() {
    let Some(server) = server() else { return };
    rtspcam_platform::install();
    let rt = Runtime::new().unwrap();
    let url = format!("rtsp://{server}/h264-720p");
    let mut opts = PipelineOptions::new(SourceOptions::from_url(&url).unwrap());
    opts.decoder = DecoderChoice::Platform;
    let p = Pipeline::start(&url, opts, rt.handle());
    let deadline = Instant::now() + Duration::from_secs(15);
    let stats = loop {
        if let StreamState::Streaming(st) = p.current_status()
            && st.frames_decoded > 30
        {
            break st;
        }
        assert!(Instant::now() < deadline, "{:?}", p.current_status());
        std::thread::sleep(Duration::from_millis(50));
    };
    assert_eq!((stats.width, stats.height), (1280, 720));
    assert!(stats.decoder.starts_with("Media Foundation"), "{stats:?}");
    assert!(stats.fps > 20.0, "{stats:?}");
    p.stop();
}
