//! The blocking client against the async server over a real named pipe.
#![cfg(all(windows, feature = "server"))]

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::time::Duration;

use rtspcam_ipc::client::{ERROR_FILE_NOT_FOUND, FrameClient};
use rtspcam_ipc::server::{FrameSource, serve};
use rtspcam_ipc::{Message, PixelFormat, StreamStatus, VideoFormat};
use uuid::Uuid;

/// Fills every byte of frame N with N.
#[derive(Default)]
struct Counter {
    seq: AtomicU64,
    connected: AtomicU32,
    disconnected: AtomicU32,
}

impl FrameSource for Counter {
    fn client_connected(&self, _: VideoFormat) {
        self.connected.fetch_add(1, Ordering::SeqCst);
    }

    fn client_disconnected(&self) {
        self.disconnected.fetch_add(1, Ordering::SeqCst);
    }

    fn next_frame(
        &self,
        format: VideoFormat,
        after: Option<u64>,
        out: &mut Vec<u8>,
    ) -> Option<u64> {
        let seq = self.seq.fetch_add(1, Ordering::SeqCst) + 1;
        if after == Some(seq) {
            return None;
        }
        out.clear();
        out.resize(format.frame_len(), seq as u8);
        Some(seq)
    }

    fn status(&self) -> (StreamStatus, String) {
        (StreamStatus::Streaming, "ok".into())
    }
}

const NV12_720P: VideoFormat = VideoFormat {
    width: 1280,
    height: 720,
    fps: 30,
    pixel_format: PixelFormat::Nv12,
};

/// Reads until `n` frames arrived; returns their formats and first bytes.
fn read_frames(client: &mut FrameClient, n: usize) -> Vec<(VideoFormat, u8, u64)> {
    let mut frames = Vec::new();
    let mut statuses = 0;
    while frames.len() < n {
        match client.read().unwrap() {
            Message::Frame { header, data } => {
                assert!(data.iter().all(|&b| b == data[0]), "frame not uniform");
                frames.push((header.format, data[0], header.seq));
            }
            Message::Status { status, .. } => {
                assert_eq!(status, StreamStatus::Streaming);
                statuses += 1;
            }
            other => panic!("unexpected {other:?}"),
        }
    }
    assert!(statuses >= 1, "no status message");
    frames
}

#[tokio::test(flavor = "multi_thread")]
async fn frames_flow_and_format_changes() {
    let id = Uuid::new_v4();
    let source = Arc::new(Counter::default());
    let server = tokio::spawn(serve(id, source.clone()));
    tokio::time::sleep(Duration::from_millis(100)).await;

    let result = tokio::task::spawn_blocking(move || {
        let mut client = FrameClient::connect(id, NV12_720P).unwrap();
        let first = read_frames(&mut client, 10);
        assert!(first.iter().all(|(f, ..)| *f == NV12_720P));
        assert!(
            first.windows(2).all(|w| w[0].2 < w[1].2),
            "seq not increasing"
        );

        let rgb = VideoFormat {
            width: 640,
            height: 480,
            fps: 15,
            pixel_format: PixelFormat::Rgb32,
        };
        client.set_format(rgb).unwrap();
        // Frames already in the pipe may still be in the old format.
        let after = read_frames(&mut client, 15);
        assert_eq!(after.last().unwrap().0, rgb);

        // A second client at the same time.
        let mut second = FrameClient::connect(id, rgb).unwrap();
        assert_eq!(read_frames(&mut second, 3)[0].0, rgb);
        second.close();
        client.close();
    })
    .await;
    result.unwrap();

    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(source.connected.load(Ordering::SeqCst), 2);
    assert_eq!(source.disconnected.load(Ordering::SeqCst), 2);
    server.abort();
}

#[test]
fn missing_pipe_is_file_not_found() {
    let err = FrameClient::connect(Uuid::new_v4(), NV12_720P).unwrap_err();
    assert_eq!(err.raw_os_error(), Some(ERROR_FILE_NOT_FOUND));
}
