//! A camera's pictures for its virtual camera backend (on Windows, served to the media source
//! DLL), scaled and converted to whatever the consumer (Discord, the Camera app, ...) asked for.

use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use rtspcam_ipc::FrameSource;
use rtspcam_ipc::{PixelFormat, StreamStatus, VideoFormat};
use rtspcam_pipeline::scale::{nv12_to_bgra, nv12_to_i420, nv12_to_yuyv};
use rtspcam_pipeline::{Frame, Matrix, Scaler};

use crate::manager::CameraShared;

pub(crate) struct CameraSource {
    camera: Arc<CameraShared>,
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    scaler: Scaler,
    /// The picture `seq` belongs to. Held (not just its address) so a new picture that reuses
    /// the old one's allocation is still seen as new.
    last: Option<Arc<Frame>>,
    seq: u64,
    /// When a frozen picture was last sent again.
    resent: Option<Instant>,
}

/// While the stream is down and the picture is held, send it again this often. The media
/// source treats a picture older than a few seconds as "no signal".
const RESEND_EVERY: Duration = Duration::from_millis(500);

impl CameraSource {
    pub(crate) fn new(camera: Arc<CameraShared>) -> Self {
        Self {
            camera,
            state: Mutex::default(),
        }
    }
}

impl FrameSource for CameraSource {
    fn client_connected(&self, format: VideoFormat) {
        tracing::info!(camera = %self.camera.name, %format, "an app started using the camera");
        self.camera.add_clients(1);
    }

    fn client_disconnected(&self) {
        tracing::info!(camera = %self.camera.name, "an app stopped using the camera");
        self.camera.add_clients(-1);
    }

    fn next_frame(
        &self,
        format: VideoFormat,
        after: Option<u64>,
        out: &mut Vec<u8>,
    ) -> Option<u64> {
        let frame = self.camera.latest_frame()?;
        let mut st = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if !st.last.as_ref().is_some_and(|l| Arc::ptr_eq(l, &frame)) {
            st.last = Some(frame.clone());
            st.seq += 1;
        }
        if after == Some(st.seq) {
            let frozen = self.camera.holds_last_frame() && !self.camera.activity().is_streaming();
            if !frozen || st.resent.is_some_and(|t| t.elapsed() < RESEND_EVERY) {
                return None;
            }
            st.seq += 1;
            st.resent = Some(Instant::now());
        }
        let fit = self.camera.fit;
        let seq = st.seq;
        match format.pixel_format {
            PixelFormat::Nv12 => {
                st.scaler
                    .scale_into(&frame, format.width, format.height, fit, out);
            }
            PixelFormat::Rgb32 => {
                let scaled = st.scaler.scale(&frame, format.width, format.height, fit);
                nv12_to_bgra(&scaled, Matrix::for_height(frame.height()), out);
            }
            PixelFormat::Yuyv => {
                let scaled = st.scaler.scale(&frame, format.width, format.height, fit);
                nv12_to_yuyv(&scaled, out);
            }
            PixelFormat::I420 => {
                let scaled = st.scaler.scale(&frame, format.width, format.height, fit);
                nv12_to_i420(&scaled, out);
            }
        }
        Some(seq)
    }

    fn status(&self) -> (StreamStatus, String) {
        self.camera.activity().ipc_status()
    }
}

#[cfg(test)]
mod tests {
    use rtspcam_core::config::{OnDisconnect, Picture, StreamConfig};
    use rtspcam_ipc::{PixelFormat, VideoFormat};
    use rtspcam_pipeline::FrameBus;

    use super::*;

    const FORMAT: VideoFormat = VideoFormat {
        width: 64,
        height: 32,
        fps: 30,
        pixel_format: PixelFormat::Nv12,
    };

    fn source(on_disconnect: OnDisconnect) -> (CameraSource, Arc<CameraShared>) {
        let mut config = StreamConfig::new("Door", "10.0.0.2");
        config.picture = Picture {
            on_disconnect,
            ..Picture::default()
        };
        let camera = Arc::new(CameraShared::new(&config, Arc::new(|| {})));
        let bus = FrameBus::new();
        bus.publish(Frame::black(64, 32));
        camera.set_bus(Some(bus));
        (CameraSource::new(camera.clone()), camera)
    }

    #[test]
    fn a_dropped_stream_stops_the_pictures() {
        let (source, camera) = source(OnDisconnect::NoSignal);
        let mut out = Vec::new();
        let seq = source.next_frame(FORMAT, None, &mut out).unwrap();
        assert_eq!(source.next_frame(FORMAT, Some(seq), &mut out), None);
        camera.set_bus(None);
        assert_eq!(source.next_frame(FORMAT, Some(seq), &mut out), None);
    }

    #[test]
    fn a_frozen_picture_is_sent_again_now_and_then() {
        let (source, camera) = source(OnDisconnect::FreezeLastFrame);
        let mut out = Vec::new();
        let seq = source.next_frame(FORMAT, None, &mut out).unwrap();

        // The stream is gone (this camera's activity is not "streaming").
        camera.set_bus(None);
        let again = source.next_frame(FORMAT, Some(seq), &mut out).unwrap();
        assert!(again > seq, "a new sequence number keeps the picture fresh");
        assert_eq!(out.len(), Frame::nv12_len(64, 32));
        // Not on every poll: the next resend is due only after a pause.
        assert_eq!(source.next_frame(FORMAT, Some(again), &mut out), None);
    }

    #[test]
    fn every_pixel_format_has_the_right_size() {
        let (source, _camera) = source(OnDisconnect::NoSignal);
        for pixel_format in [
            PixelFormat::Nv12,
            PixelFormat::Rgb32,
            PixelFormat::Yuyv,
            PixelFormat::I420,
        ] {
            let format = VideoFormat {
                pixel_format,
                ..FORMAT
            };
            let mut out = Vec::new();
            source.next_frame(format, None, &mut out).unwrap();
            assert_eq!(out.len(), format.frame_len(), "{format}");
        }
    }
}
