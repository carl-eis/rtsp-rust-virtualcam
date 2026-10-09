//! The blocking client against the async server over this OS's real transport (named pipes on
//! Windows, Unix sockets elsewhere).

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::time::Duration;

use rtspcam_ipc::client::FrameClient;
use rtspcam_ipc::server::{FrameSource, serve};
use rtspcam_ipc::{Message, PixelFormat, StreamStatus, VideoFormat};
use rtspcam_platform::{ReadWrite, frame_transport};
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

/// Reads until `n` frames arrived; returns their formats and sequence numbers.
fn read_frames(client: &mut FrameClient<Box<dyn ReadWrite>>, n: usize) -> Vec<(VideoFormat, u64)> {
    let mut frames = Vec::new();
    while frames.len() < n {
        match client.read().unwrap() {
            Message::Frame { header, data } => {
                assert!(data.iter().all(|&b| b == data[0]), "frame not uniform");
                frames.push((header.format, header.seq));
            }
            Message::Status { status, .. } => assert_eq!(status, StreamStatus::Streaming),
            other => panic!("unexpected {other:?}"),
        }
    }
    frames
}

#[tokio::test(flavor = "multi_thread")]
async fn frames_flow_over_the_platform_transport() {
    let transport = frame_transport();
    let id = Uuid::new_v4();
    assert!(!transport.is_served(id));
    let source = Arc::new(Counter::default());
    let server = tokio::spawn(serve(transport.listen(id).unwrap(), source.clone()));
    assert!(transport.is_served(id));
    // Only one server per camera.
    assert!(transport.listen(id).is_err());

    let client_transport = transport.clone();
    let result = tokio::task::spawn_blocking(move || {
        let connect = |format| FrameClient::new(client_transport.connect(id).unwrap(), format);
        let mut client = connect(NV12_720P).unwrap();
        let first = read_frames(&mut client, 10);
        assert!(first.iter().all(|(f, _)| *f == NV12_720P));

        let rgb = VideoFormat {
            width: 640,
            height: 480,
            fps: 15,
            pixel_format: PixelFormat::Rgb32,
        };
        client.set_format(rgb).unwrap();
        let after = read_frames(&mut client, 15);
        assert_eq!(after.last().unwrap().0, rgb);

        // A second client at the same time.
        let mut second = connect(rgb).unwrap();
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
    let _ = server.await;
    // Give the OS a moment to tear the endpoint down.
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    while transport.is_served(id) {
        assert!(std::time::Instant::now() < deadline, "still served");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[test]
fn connecting_to_an_unserved_camera_fails() {
    assert!(frame_transport().connect(Uuid::new_v4()).is_err());
}
