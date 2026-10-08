//! RTSP ingest against the mediamtx test server. See `common/mod.rs` for how to run.

mod common;

use std::time::Duration;

use rtspcam_pipeline::{ErrorKind, RtspSource, SourceOptions, VideoCodec};

async fn receive(url: &str, frames: usize) -> (RtspSource, Vec<rtspcam_pipeline::EncodedFrame>) {
    let opts = SourceOptions::from_url(url).unwrap();
    let mut source = RtspSource::connect(&opts).await.unwrap();
    let mut got = Vec::new();
    while got.len() < frames {
        let f = tokio::time::timeout(Duration::from_secs(5), source.next_frame())
            .await
            .expect("frame within 5 s")
            .unwrap()
            .expect("stream still open");
        got.push(f);
    }
    (source, got)
}

#[tokio::test]
async fn receives_every_test_stream() {
    let Some(server) = common::server() else {
        return;
    };
    let cases = [
        ("h264-720p", VideoCodec::H264, (1280, 720)),
        ("h264-1080p", VideoCodec::H264, (1920, 1080)),
        ("h265-720p", VideoCodec::H265, (1280, 720)),
    ];
    for (path, codec, size) in cases {
        let (source, frames) = receive(&common::url(&server, path), 40).await;
        let info = source.info();
        assert_eq!(info.codec, codec, "{path}");
        assert_eq!(info.size, Some(size), "{path}");
        // The test streams have a key frame every 30 frames. (mjpeg-720p is left out: see the
        // known issue in tools/test-rtsp/README.md.)
        assert!(frames.iter().any(|f| f.keyframe), "{path}: no key frame");
        assert!(
            frames
                .iter()
                .all(|f| f.codec == codec && !f.data.is_empty())
        );
        assert!(
            frames.windows(2).all(|w| w[0].pts <= w[1].pts),
            "{path}: timestamps go backwards"
        );
        if codec != VideoCodec::Mjpeg {
            let key = frames.iter().find(|f| f.keyframe).unwrap();
            assert!(key.data.starts_with(&[0, 0, 0, 1]), "{path}: not Annex B");
        }
    }
}

#[tokio::test]
async fn secure_stream_needs_credentials() {
    let Some(server) = common::server() else {
        return;
    };
    let (source, _) = receive(&common::secure_url(&server), 5).await;
    assert_eq!(source.info().codec, VideoCodec::H264);

    let opts = SourceOptions::from_url(&common::url(&server, "secure")).unwrap();
    let err = RtspSource::connect(&opts).await.unwrap_err();
    assert_eq!(err.kind(), ErrorKind::Unauthorized, "{err}");

    let opts = SourceOptions::from_url(&format!("rtsp://rtspcam:wrong@{server}/secure")).unwrap();
    let err = RtspSource::connect(&opts).await.unwrap_err();
    assert_eq!(err.kind(), ErrorKind::Unauthorized, "{err}");
}

#[tokio::test]
async fn classifies_connection_errors() {
    let Some(server) = common::server() else {
        return;
    };
    // mediamtx answers 400 rather than 404 for a path it isn't configured for.
    let opts = SourceOptions::from_url(&common::url(&server, "no-such-path")).unwrap();
    let err = RtspSource::connect(&opts).await.unwrap_err();
    assert!(
        matches!(err.kind(), ErrorKind::NotFound | ErrorKind::Protocol),
        "{err}"
    );

    // Nothing listens on port 1.
    let opts = SourceOptions::from_url("rtsp://127.0.0.1:1/x").unwrap();
    let err = RtspSource::connect(&opts).await.unwrap_err();
    assert_eq!(err.kind(), ErrorKind::Unreachable, "{err}");
}

/// Opt-in with `RTSPCAM_TEST_UDP=1`: Docker Desktop's port forwarding doesn't carry the
/// server's RTP/UDP packets back to the host, so this only passes with a native mediamtx.
#[tokio::test]
async fn udp_transport() {
    let Some(server) = common::server() else {
        return;
    };
    if std::env::var_os("RTSPCAM_TEST_UDP").is_none() {
        eprintln!("RTSPCAM_TEST_UDP not set; skipping");
        return;
    }
    let mut opts = SourceOptions::from_url(&common::url(&server, "h264-720p")).unwrap();
    opts.transport = rtspcam_core::Transport::Udp;
    let mut source = RtspSource::connect(&opts).await.unwrap();
    let frame = tokio::time::timeout(Duration::from_secs(5), source.next_frame())
        .await
        .expect("frame within 5 s")
        .unwrap();
    assert!(frame.is_some());
}
