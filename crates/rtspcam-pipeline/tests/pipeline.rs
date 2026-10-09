//! Full pipelines (ingest + decode + bus) against the mediamtx test server. See
//! `common/mod.rs` for how to run.

mod common;

use std::time::{Duration, Instant};

use rtspcam_pipeline::decode::DecoderChoice;
use rtspcam_pipeline::{ErrorKind, Pipeline, PipelineOptions, SourceOptions, StreamState};
use tokio::runtime::Runtime;

fn start(rt: &Runtime, url: &str, decoder: DecoderChoice) -> Pipeline {
    let mut opts = PipelineOptions::new(SourceOptions::from_url(url).unwrap());
    opts.decoder = decoder;
    Pipeline::start(url, opts, rt.handle())
}

/// Waits until `pred` holds for the pipeline's state, or panics after `timeout`.
fn wait_for(p: &Pipeline, timeout: Duration, pred: impl Fn(&StreamState) -> bool) -> StreamState {
    let deadline = Instant::now() + timeout;
    loop {
        let s = p.current_status();
        if pred(&s) {
            return s;
        }
        assert!(Instant::now() < deadline, "timed out; last state: {s:?}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn assert_streams(rt: &Runtime, url: &str, decoder: DecoderChoice, size: (u32, u32)) {
    let p = start(rt, url, decoder);
    // Wait for a full second of stats so fps is meaningful.
    let state = wait_for(
        &p,
        Duration::from_secs(15),
        |s| matches!(s, StreamState::Streaming(st) if st.frames_decoded > 30),
    );
    let StreamState::Streaming(stats) = state else {
        unreachable!()
    };
    assert_eq!((stats.width, stats.height), size, "{url}");
    assert!(stats.fps > 20.0, "{url}: {stats:?}");
    assert!(stats.bitrate > 100_000, "{url}: {stats:?}");
    assert!(
        stats.latency < Duration::from_millis(150),
        "{url}: {stats:?}"
    );
    let frame = p.frames().latest().expect("a picture on the bus");
    assert_eq!((frame.width(), frame.height()), size);
    eprintln!("{url}: {stats:?}");
    p.stop();
}

/// The platform decoders are not installed in this test binary, so `Auto` means OpenH264 here
/// (`rtspcam-platform` runs the same streams through Media Foundation on Windows).
#[test]
fn streams_h264() {
    let Some(server) = common::server() else {
        return;
    };
    let rt = Runtime::new().unwrap();
    let url = common::url(&server, "h264-720p");
    assert_streams(&rt, &url, DecoderChoice::OpenH264, (1280, 720));
    assert_streams(
        &rt,
        &common::url(&server, "h264-1080p"),
        DecoderChoice::Auto,
        (1920, 1080),
    );
    assert_streams(
        &rt,
        &common::secure_url(&server),
        DecoderChoice::Auto,
        (1280, 720),
    );
}

/// Only platform decoders handle H.265 (on Windows with Microsoft's "HEVC Video Extensions").
/// Without one the pipeline must report `DecoderUnavailable`.
#[test]
fn h265_streams_or_reports_missing_decoder() {
    let Some(server) = common::server() else {
        return;
    };
    let rt = Runtime::new().unwrap();
    let p = start(&rt, &common::url(&server, "h265-720p"), DecoderChoice::Auto);
    let state = wait_for(&p, Duration::from_secs(15), |s| match s {
        StreamState::Streaming(st) => st.frames_decoded > 30,
        StreamState::Retrying { .. } => true,
        _ => false,
    });
    match state {
        StreamState::Streaming(st) => assert_eq!((st.width, st.height), (1280, 720)),
        StreamState::Retrying { error, .. } => {
            assert_eq!(error.kind(), ErrorKind::DecoderUnavailable, "{error}");
            eprintln!("no HEVC decoder on this machine: {error}");
        }
        _ => unreachable!(),
    }
}

#[test]
fn wrong_password_retries_with_unauthorized() {
    let Some(server) = common::server() else {
        return;
    };
    let rt = Runtime::new().unwrap();
    let p = start(
        &rt,
        &format!("rtsp://rtspcam:wrong@{server}/secure"),
        DecoderChoice::Auto,
    );
    let state = wait_for(&p, Duration::from_secs(10), |s| {
        matches!(s, StreamState::Retrying { .. })
    });
    let StreamState::Retrying {
        error, retry_at, ..
    } = state
    else {
        unreachable!()
    };
    assert_eq!(error.kind(), ErrorKind::Unauthorized);
    // Not worth hammering the camera: permanent errors wait the maximum backoff.
    assert!(retry_at > Instant::now() + Duration::from_secs(20));
    assert!(p.frames().latest().is_none());
}

#[test]
fn stop_is_prompt() {
    let Some(server) = common::server() else {
        return;
    };
    let rt = Runtime::new().unwrap();
    let p = start(&rt, &common::url(&server, "h264-720p"), DecoderChoice::Auto);
    wait_for(&p, Duration::from_secs(15), StreamState::is_streaming);
    let t = Instant::now();
    p.stop();
    assert!(
        t.elapsed() < Duration::from_secs(1),
        "stop took {:?}",
        t.elapsed()
    );
}
