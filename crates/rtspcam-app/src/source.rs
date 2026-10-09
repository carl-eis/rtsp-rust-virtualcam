//! Serves a camera's pictures to the media source DLL, scaled and converted to whatever the
//! consumer (Discord, the Camera app, ...) asked for.

use std::sync::{Arc, Mutex, PoisonError};

use rtspcam_ipc::server::FrameSource;
use rtspcam_ipc::{PixelFormat, StreamStatus, VideoFormat};
use rtspcam_pipeline::scale::nv12_to_bgra;
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
        let frame = self.camera.latest_frame()?;
        let mut st = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if !st.last.as_ref().is_some_and(|l| Arc::ptr_eq(l, &frame)) {
            st.last = Some(frame.clone());
            st.seq += 1;
        }
        if after == Some(st.seq) {
            return None;
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
        }
        Some(seq)
    }

    fn status(&self) -> (StreamStatus, String) {
        self.camera.activity().ipc_status()
    }
}
