//! A camera's pictures for its virtual camera backend (on Windows, served to the media source
//! DLL), scaled and converted to whatever the consumer (Discord, the Camera app, ...) asked for.
//!
//! Each output format asked for has its own scaler and keeps its last result, so two apps
//! reading the same format get one scaling between them, and apps reading different formats
//! neither wait for each other nor make the scaler start over.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use rtspcam_core::profile::{self, Stage};
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
    /// The picture `seq` belongs to. Held (not just its address) so a new picture that reuses
    /// the old one's allocation is still seen as new.
    last: Option<Arc<Frame>>,
    seq: u64,
    /// When a frozen picture was last sent again.
    resent: Option<Instant>,
    outputs: HashMap<(u32, u32, PixelFormat), Arc<Mutex<Output>>>,
}

/// One output format's scaler and its last result.
///
/// The result is kept (and copied out to each reader) only while two or more readers share the
/// format; a lone reader gets it written straight into its buffer, without a copy.
#[derive(Default)]
struct Output {
    scaler: Scaler,
    /// The NV12 picture at the output size, for formats converted from it.
    scaled: Vec<u8>,
    /// The picture last made, and whether `bytes` holds the result.
    made_from: Option<Arc<Frame>>,
    cached: bool,
    bytes: Vec<u8>,
    /// Another reader wanted the same picture: the last one, and the one before.
    shared_now: bool,
    shared_before: bool,
    used: Option<Instant>,
}

/// While the stream is down and the picture is held, send it again this often. The media
/// source treats a picture older than a few seconds as "no signal".
const RESEND_EVERY: Duration = Duration::from_millis(500);

/// An output format nobody asked for this long is dropped (its app left or switched).
const OUTPUT_IDLE: Duration = Duration::from_secs(10);

impl Output {
    /// Writes `frame` in `format` into `out`.
    fn fill(
        &mut self,
        frame: &Arc<Frame>,
        format: VideoFormat,
        fit: rtspcam_core::FitMode,
        out: &mut Vec<u8>,
    ) {
        self.used = Some(Instant::now());
        let same = self
            .made_from
            .as_ref()
            .is_some_and(|f| Arc::ptr_eq(f, frame));
        if same {
            // A second reader of this picture.
            self.shared_now = true;
            if !self.cached {
                self.make(frame, format, fit, None);
                self.cached = true;
            }
            out.clear();
            out.extend_from_slice(&self.bytes);
            return;
        }
        // A new picture: keep it for others only if the last one was shared.
        self.shared_before = std::mem::take(&mut self.shared_now);
        self.made_from = Some(frame.clone());
        self.cached = self.shared_before;
        if self.cached {
            self.make(frame, format, fit, None);
            out.clear();
            out.extend_from_slice(&self.bytes);
        } else {
            self.make(frame, format, fit, Some(out));
        }
    }

    /// Scales and converts `frame` into `out`, or into `bytes` when `None`.
    fn make(
        &mut self,
        frame: &Frame,
        format: VideoFormat,
        fit: rtspcam_core::FitMode,
        out: Option<&mut Vec<u8>>,
    ) {
        let out = match out {
            Some(out) => out,
            None => &mut self.bytes,
        };
        let (w, h) = (format.width, format.height);
        if format.pixel_format == PixelFormat::Nv12 {
            let _t = profile::time(Stage::Scale);
            self.scaler.scale_into(frame, w, h, fit, out);
            return;
        }
        {
            let _t = profile::time(Stage::Scale);
            self.scaler.scale_into(frame, w, h, fit, &mut self.scaled);
        }
        let _t = profile::time(Stage::Convert);
        // Borrow the buffer as a Frame and give it back, so nothing is allocated.
        let mut scaled = Frame::from_nv12(w, h, std::mem::take(&mut self.scaled));
        scaled.source_size = frame.source_size;
        match format.pixel_format {
            PixelFormat::Nv12 => unreachable!("handled above"),
            PixelFormat::Rgb32 => nv12_to_bgra(&scaled, Matrix::for_frame(&scaled), out),
            PixelFormat::Yuyv => nv12_to_yuyv(&scaled, out),
            PixelFormat::I420 => nv12_to_i420(&scaled, out),
        }
        self.scaled = scaled.into_data();
    }
}

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
        self.camera
            .note_request(format.width, format.height, self.camera.fit);
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
        let seq = st.seq;
        let key = (format.width, format.height, format.pixel_format);
        if !st.outputs.contains_key(&key) {
            st.outputs.retain(|_, o| {
                o.lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .used
                    .is_some_and(|t| t.elapsed() < OUTPUT_IDLE)
            });
        }
        let output = st.outputs.entry(key).or_default().clone();
        // Other formats go on while this one is made.
        drop(st);
        let mut output = output.lock().unwrap_or_else(PoisonError::into_inner);
        output.fill(&frame, format, self.camera.fit, out);
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

    #[test]
    fn each_format_is_made_once_per_picture() {
        let (source, _camera) = source(OnDisconnect::NoSignal);
        let rgb = VideoFormat {
            pixel_format: PixelFormat::Rgb32,
            ..FORMAT
        };
        let (mut a, mut b, mut c) = (Vec::new(), Vec::new(), Vec::new());
        source.next_frame(FORMAT, None, &mut a).unwrap();
        source.next_frame(rgb, None, &mut b).unwrap();
        source.next_frame(FORMAT, None, &mut c).unwrap();
        assert_eq!(a, c, "the second reader of a format gets the same bytes");
        assert_eq!(b.len(), rgb.frame_len());
        let st = source.state.lock().unwrap();
        assert_eq!(st.outputs.len(), 2, "one scaler per format");
        let nv12 = st.outputs[&(64, 32, PixelFormat::Nv12)].lock().unwrap();
        assert!(
            nv12.cached && nv12.shared_now,
            "kept once a second reader came"
        );
    }
}
