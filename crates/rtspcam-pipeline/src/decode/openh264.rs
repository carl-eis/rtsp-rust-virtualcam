//! Cisco OpenH264 software decoder: the H.264 fallback when Media Foundation isn't usable.

use openh264::formats::YUVSource as _;

use super::{ArrivalLog, Decoder, stamp};
use crate::error::{ErrorKind, PipelineError};
use crate::frame::{Chroma, Yuv420Planes};
use crate::{EncodedFrame, Frame, VideoCodec};

pub struct OpenH264Decoder {
    decoder: openh264::decoder::Decoder,
    arrivals: ArrivalLog,
}

impl std::fmt::Debug for OpenH264Decoder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpenH264Decoder").finish_non_exhaustive()
    }
}

impl OpenH264Decoder {
    pub fn new(codec: VideoCodec) -> Result<Self, PipelineError> {
        if codec != VideoCodec::H264 {
            return Err(PipelineError::new(
                ErrorKind::DecoderUnavailable,
                format!("OpenH264 can't decode {codec}"),
            ));
        }
        let decoder = openh264::decoder::Decoder::new().map_err(|e| {
            PipelineError::new(ErrorKind::DecoderUnavailable, format!("OpenH264: {e}"))
        })?;
        Ok(Self {
            decoder,
            arrivals: ArrivalLog::default(),
        })
    }
}

impl Decoder for OpenH264Decoder {
    fn name(&self) -> &str {
        "OpenH264"
    }

    fn decode(&mut self, frame: &EncodedFrame, out: &mut Vec<Frame>) -> Result<(), PipelineError> {
        self.arrivals.push(frame.pts, frame.received);
        let decoded = self
            .decoder
            .decode(&frame.data)
            .map_err(|e| PipelineError::new(ErrorKind::Decode, format!("OpenH264: {e}")))?;
        let Some(yuv) = decoded else {
            return Ok(());
        };
        let (w, h) = yuv.dimensions();
        let (y_stride, u_stride, _) = yuv.strides();
        let planes = Yuv420Planes {
            y: yuv.y(),
            y_stride,
            chroma: Chroma::Planar {
                u: yuv.u(),
                v: yuv.v(),
                stride: u_stride,
            },
            width: w as u32,
            height: h as u32,
        };
        if let Some(picture) = planes.to_frame() {
            // OpenH264 has no reordering delay in the baseline/main profiles cameras use, so the
            // output belongs to the frame just submitted.
            out.push(stamp(picture, frame.pts, &self.arrivals));
        }
        Ok(())
    }
}
