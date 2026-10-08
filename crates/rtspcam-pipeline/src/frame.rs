//! Decoded video frames.

use std::time::{Duration, Instant};

/// Luma value of black in limited-range (video) YUV.
pub const BLACK_Y: u8 = 16;
/// Neutral chroma value.
pub const NEUTRAL_UV: u8 = 128;

/// One decoded picture in NV12: a full-resolution Y plane followed by an interleaved,
/// half-resolution UV plane. Rows are tightly packed (stride == width), and width and height
/// are always even.
#[derive(Clone)]
pub struct Frame {
    width: u32,
    height: u32,
    data: Vec<u8>,
    /// Presentation time from the stream (RTP timestamp, relative to the session start).
    pub pts: Duration,
    /// When the last network packet of this frame arrived.
    pub received: Instant,
    /// When the decoder produced it.
    pub decoded: Instant,
}

impl Frame {
    /// Bytes needed for an NV12 picture of this size.
    pub const fn nv12_len(width: u32, height: u32) -> usize {
        width as usize * height as usize * 3 / 2
    }

    /// Wraps NV12 data.
    ///
    /// # Panics
    /// If a dimension is odd or zero, or `data` has the wrong length.
    pub fn from_nv12(width: u32, height: u32, data: Vec<u8>) -> Self {
        assert!(
            width > 0 && height > 0 && width % 2 == 0 && height % 2 == 0,
            "NV12 frames need even, non-zero dimensions (got {width}x{height})"
        );
        assert_eq!(
            data.len(),
            Self::nv12_len(width, height),
            "NV12 buffer size"
        );
        let now = Instant::now();
        Self {
            width,
            height,
            data,
            pts: Duration::ZERO,
            received: now,
            decoded: now,
        }
    }

    /// A black picture.
    pub fn black(width: u32, height: u32) -> Self {
        let mut data = vec![NEUTRAL_UV; Self::nv12_len(width, height)];
        data[..width as usize * height as usize].fill(BLACK_Y);
        Self::from_nv12(width, height, data)
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    /// The whole NV12 buffer.
    pub fn data(&self) -> &[u8] {
        &self.data
    }

    pub fn into_data(self) -> Vec<u8> {
        self.data
    }

    /// The Y plane (`width * height` bytes).
    pub fn y(&self) -> &[u8] {
        &self.data[..self.width as usize * self.height as usize]
    }

    /// The interleaved UV plane (`width * height / 2` bytes, `width` bytes per row).
    pub fn uv(&self) -> &[u8] {
        &self.data[self.width as usize * self.height as usize..]
    }

    /// Time from the last packet arriving to the decoded picture being ready.
    pub fn decode_latency(&self) -> Duration {
        self.decoded.saturating_duration_since(self.received)
    }
}

impl std::fmt::Debug for Frame {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Frame")
            .field("width", &self.width)
            .field("height", &self.height)
            .field("pts", &self.pts)
            .finish_non_exhaustive()
    }
}

/// Copies a cropped region of a planar or semi-planar 4:2:0 picture into a new NV12 [`Frame`].
///
/// Decoders hand out pictures with padding (for example 1920x1088 with a 1920x1080 visible area,
/// or rows wider than the picture). `width` and `height` are rounded down to even values.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Yuv420Planes<'a> {
    pub y: &'a [u8],
    pub y_stride: usize,
    pub chroma: Chroma<'a>,
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum Chroma<'a> {
    /// NV12: interleaved U, V.
    Interleaved { uv: &'a [u8], stride: usize },
    /// I420 / YV12: separate U and V planes.
    Planar {
        u: &'a [u8],
        v: &'a [u8],
        stride: usize,
    },
}

impl Yuv420Planes<'_> {
    pub(crate) fn to_frame(self) -> Option<Frame> {
        let w = (self.width & !1) as usize;
        let h = (self.height & !1) as usize;
        if w == 0 || h == 0 {
            return None;
        }
        let mut data = vec![0u8; w * h * 3 / 2];
        let (dst_y, dst_uv) = data.split_at_mut(w * h);
        for row in 0..h {
            let src = self.y.get(row * self.y_stride..row * self.y_stride + w)?;
            dst_y[row * w..(row + 1) * w].copy_from_slice(src);
        }
        let cw = w / 2;
        for row in 0..h / 2 {
            let dst = &mut dst_uv[row * w..(row + 1) * w];
            match self.chroma {
                Chroma::Interleaved { uv, stride } => {
                    dst.copy_from_slice(uv.get(row * stride..row * stride + w)?);
                }
                Chroma::Planar { u, v, stride } => {
                    let u = u.get(row * stride..row * stride + cw)?;
                    let v = v.get(row * stride..row * stride + cw)?;
                    for (i, pair) in dst.chunks_exact_mut(2).enumerate() {
                        pair[0] = u[i];
                        pair[1] = v[i];
                    }
                }
            }
        }
        Some(Frame::from_nv12(w as u32, h as u32, data))
    }
}

/// Converts a packed YUY2 (YUYV 4:2:2) picture to NV12 by averaging chroma of row pairs.
pub(crate) fn yuy2_to_frame(src: &[u8], stride: usize, width: u32, height: u32) -> Option<Frame> {
    let w = (width & !1) as usize;
    let h = (height & !1) as usize;
    if w == 0 || h == 0 {
        return None;
    }
    let mut data = vec![0u8; w * h * 3 / 2];
    let (dst_y, dst_uv) = data.split_at_mut(w * h);
    for row in 0..h {
        let line = src.get(row * stride..row * stride + w * 2)?;
        for (x, px) in line.chunks_exact(2).enumerate() {
            dst_y[row * w + x] = px[0];
        }
    }
    for row in 0..h / 2 {
        let a = src.get(2 * row * stride..2 * row * stride + w * 2)?;
        let b = src.get((2 * row + 1) * stride..(2 * row + 1) * stride + w * 2)?;
        let dst = &mut dst_uv[row * w..(row + 1) * w];
        // Each 4-byte group Y0 U Y1 V covers two pixels -> one UV pair.
        for (i, (ga, gb)) in a.chunks_exact(4).zip(b.chunks_exact(4)).enumerate() {
            dst[2 * i] = avg(ga[1], gb[1]);
            dst[2 * i + 1] = avg(ga[3], gb[3]);
        }
    }
    Some(Frame::from_nv12(w as u32, h as u32, data))
}

fn avg(a: u8, b: u8) -> u8 {
    (u16::from(a) + u16::from(b)).div_ceil(2) as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn black_frame_layout() {
        let f = Frame::black(4, 2);
        assert_eq!(f.y(), &[BLACK_Y; 8]);
        assert_eq!(f.uv(), &[NEUTRAL_UV; 4]);
    }

    #[test]
    #[should_panic(expected = "even")]
    fn odd_size_is_rejected() {
        let _ = Frame::from_nv12(3, 2, vec![0; 9]);
    }

    #[test]
    fn crops_padded_nv12() {
        // 4x2 visible inside a 6-wide, 4-high padded buffer.
        let y: Vec<u8> = (0..24).collect();
        let uv: Vec<u8> = (100..112).collect();
        let f = Yuv420Planes {
            y: &y,
            y_stride: 6,
            chroma: Chroma::Interleaved { uv: &uv, stride: 6 },
            width: 5, // rounded down to 4
            height: 2,
        }
        .to_frame()
        .unwrap();
        assert_eq!((f.width(), f.height()), (4, 2));
        assert_eq!(f.y(), &[0, 1, 2, 3, 6, 7, 8, 9]);
        assert_eq!(f.uv(), &[100, 101, 102, 103]);
    }

    #[test]
    fn interleaves_i420() {
        let y = [1u8; 8];
        let u = [10u8, 11];
        let v = [20u8, 21];
        let f = Yuv420Planes {
            y: &y,
            y_stride: 4,
            chroma: Chroma::Planar {
                u: &u,
                v: &v,
                stride: 2,
            },
            width: 4,
            height: 2,
        }
        .to_frame()
        .unwrap();
        assert_eq!(f.uv(), &[10, 20, 11, 21]);
    }

    #[test]
    fn yuy2_conversion() {
        // 2x2: row 0 = Y 1,2 U 10 V 20; row 1 = Y 3,4 U 30 V 40.
        let src = [1, 10, 2, 20, 3, 30, 4, 40];
        let f = yuy2_to_frame(&src, 4, 2, 2).unwrap();
        assert_eq!(f.y(), &[1, 2, 3, 4]);
        assert_eq!(f.uv(), &[20, 30]);
    }

    #[test]
    fn short_buffer_is_none() {
        let y = [0u8; 4];
        let uv = [0u8; 2];
        let planes = Yuv420Planes {
            y: &y,
            y_stride: 4,
            chroma: Chroma::Interleaved { uv: &uv, stride: 4 },
            width: 4,
            height: 2,
        };
        assert!(planes.to_frame().is_none());
    }
}
