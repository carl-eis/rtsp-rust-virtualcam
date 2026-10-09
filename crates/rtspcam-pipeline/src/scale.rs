//! CPU scaling of NV12 pictures to a target size, and NV12 → BGRA conversion for the preview.
//!
//! Bilinear filtering with 8-bit fixed-point weights. The GPU path (Video Processor MFT) is a
//! Phase 8 optimization; this one has no dependencies and works everywhere.

use rtspcam_core::FitMode;

use crate::Frame;
use crate::frame::{BLACK_Y, NEUTRAL_UV};

/// A rectangle in pixels. Positions and sizes produced by [`fit`] are always even, so they map
/// exactly onto NV12's half-resolution chroma.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

impl Rect {
    pub const fn new(x: u32, y: u32, w: u32, h: u32) -> Self {
        Self { x, y, w, h }
    }
}

/// Works out which part of the source is shown where in the destination.
///
/// - `Letterbox`: the whole source, scaled to fit, centered with black bars.
/// - `Crop`: fills the destination, cutting off the edges of the source that don't fit.
/// - `Stretch`: the whole source into the whole destination, ignoring aspect ratio.
///
/// Returns `(source rect, destination rect)`.
pub fn fit(src_w: u32, src_h: u32, dst_w: u32, dst_h: u32, mode: FitMode) -> (Rect, Rect) {
    let full_src = Rect::new(0, 0, src_w, src_h);
    let full_dst = Rect::new(0, 0, dst_w, dst_h);
    let (sw, sh, dw, dh) = (
        u64::from(src_w),
        u64::from(src_h),
        u64::from(dst_w),
        u64::from(dst_h),
    );
    match mode {
        FitMode::Stretch => (full_src, full_dst),
        FitMode::Letterbox => {
            // Compare aspect ratios without floating point: sw/sh vs dw/dh.
            let (w, h) = if sw * dh > dw * sh {
                (dw, even_round(dw * sh, sw))
            } else {
                (even_round(dh * sw, sh), dh)
            };
            let (w, h) = (w.clamp(2, dw) as u32, h.clamp(2, dh) as u32);
            (
                full_src,
                Rect::new(even(dst_w - w) / 2, even(dst_h - h) / 2, w, h),
            )
        }
        FitMode::Crop => {
            let (w, h) = if sw * dh > dw * sh {
                (even_round(sh * dw, dh), sh)
            } else {
                (sw, even_round(sw * dh, dw))
            };
            let (w, h) = (w.clamp(2, sw) as u32, h.clamp(2, sh) as u32);
            (
                Rect::new(even((src_w - w) / 2), even((src_h - h) / 2), w, h),
                full_dst,
            )
        }
    }
}

/// `num / den`, rounded to the nearest even number.
fn even_round(num: u64, den: u64) -> u64 {
    ((num + den) / (2 * den)) * 2
}

fn even(v: u32) -> u32 {
    v & !1
}

/// Reusable NV12 scaler. Keeps its coefficient tables between calls with the same geometry.
#[derive(Debug, Default)]
pub struct Scaler {
    plan: Option<Plan>,
}

#[derive(Debug, PartialEq)]
struct PlanKey {
    src: (u32, u32),
    dst: (u32, u32),
    mode: FitMode,
}

#[derive(Debug)]
struct Plan {
    key: PlanKey,
    src_rect: Rect,
    dst_rect: Rect,
    luma: Axes,
    chroma: Axes,
}

#[derive(Debug)]
struct Axes {
    x: Vec<Tap>,
    y: Vec<Tap>,
}

/// One output coordinate: blend `i0` and `i1` with weight `w` (0..=256) on `i1`.
#[derive(Debug, Clone, Copy)]
struct Tap {
    i0: u32,
    i1: u32,
    w: u32,
}

fn taps(src_off: u32, src_len: u32, dst_len: u32) -> Vec<Tap> {
    let scale = f64::from(src_len) / f64::from(dst_len);
    let last = src_len - 1;
    (0..dst_len)
        .map(|d| {
            let pos = ((f64::from(d) + 0.5) * scale - 0.5).max(0.0);
            let i0 = (pos.floor() as u32).min(last);
            let i1 = (i0 + 1).min(last);
            let w = ((pos - f64::from(i0)) * 256.0).round().clamp(0.0, 256.0) as u32;
            Tap {
                i0: src_off + i0,
                i1: src_off + i1,
                w,
            }
        })
        .collect()
}

impl Scaler {
    pub fn new() -> Self {
        Self::default()
    }

    /// Scales `src` into a new `dst_w` x `dst_h` frame. Timestamps are copied from `src`.
    pub fn scale(&mut self, src: &Frame, dst_w: u32, dst_h: u32, mode: FitMode) -> Frame {
        let mut data = Vec::new();
        self.scale_into(src, dst_w, dst_h, mode, &mut data);
        let mut out = Frame::from_nv12(dst_w, dst_h, data);
        out.pts = src.pts;
        out.received = src.received;
        out.decoded = src.decoded;
        out
    }

    /// Scales `src` into `out` as NV12 at `dst_w` x `dst_h` (both even), reusing its allocation.
    pub fn scale_into(
        &mut self,
        src: &Frame,
        dst_w: u32,
        dst_h: u32,
        mode: FitMode,
        out: &mut Vec<u8>,
    ) {
        assert!(dst_w.is_multiple_of(2) && dst_h.is_multiple_of(2) && dst_w > 0 && dst_h > 0);
        let plan = self.plan_for(src.width(), src.height(), dst_w, dst_h, mode);
        let (dw, dh) = (dst_w as usize, dst_h as usize);
        out.resize(Frame::nv12_len(dst_w, dst_h), 0);
        let (out_y, out_uv) = out.split_at_mut(dw * dh);

        let r = plan.dst_rect;
        if r != Rect::new(0, 0, dst_w, dst_h) {
            out_y.fill(BLACK_Y);
            out_uv.fill(NEUTRAL_UV);
        }

        let sw = src.width() as usize;
        scale_plane::<1>(src.y(), sw, out_y, dw, &plan.luma, plan.src_rect, r);
        let chroma_dst = Rect::new(r.x / 2, r.y / 2, r.w / 2, r.h / 2);
        let chroma_src = Rect::new(
            plan.src_rect.x / 2,
            plan.src_rect.y / 2,
            plan.src_rect.w / 2,
            plan.src_rect.h / 2,
        );
        scale_plane::<2>(
            src.uv(),
            sw,
            out_uv,
            dw,
            &plan.chroma,
            chroma_src,
            chroma_dst,
        );
    }

    fn plan_for(&mut self, sw: u32, sh: u32, dw: u32, dh: u32, mode: FitMode) -> &Plan {
        let key = PlanKey {
            src: (sw, sh),
            dst: (dw, dh),
            mode,
        };
        if self.plan.as_ref().is_none_or(|p| p.key != key) {
            let (s, d) = fit(sw, sh, dw, dh, mode);
            self.plan = Some(Plan {
                key,
                src_rect: s,
                dst_rect: d,
                luma: Axes {
                    x: taps(s.x, s.w, d.w),
                    y: taps(s.y, s.h, d.h),
                },
                chroma: Axes {
                    x: taps(s.x / 2, s.w / 2, d.w / 2),
                    y: taps(s.y / 2, s.h / 2, d.h / 2),
                },
            });
        }
        self.plan.as_ref().expect("plan was just set")
    }
}

/// Scales one plane. `C` is the number of interleaved channels (1 for Y, 2 for UV). Strides
/// are in bytes; rect coordinates are in samples of this plane.
fn scale_plane<const C: usize>(
    src: &[u8],
    src_stride: usize,
    dst: &mut [u8],
    dst_stride: usize,
    axes: &Axes,
    src_rect: Rect,
    dst_rect: Rect,
) {
    let (dx, dst_y) = (dst_rect.x as usize * C, dst_rect.y as usize);
    let row_len = dst_rect.w as usize * C;

    if src_rect.w == dst_rect.w && src_rect.h == dst_rect.h {
        let sx = src_rect.x as usize * C;
        for row in 0..dst_rect.h as usize {
            let s = (src_rect.y as usize + row) * src_stride + sx;
            let d = (dst_y + row) * dst_stride + dx;
            dst[d..d + row_len].copy_from_slice(&src[s..s + row_len]);
        }
        return;
    }

    for (row, ty) in axes.y.iter().enumerate() {
        let r0 = &src[ty.i0 as usize * src_stride..];
        let r1 = &src[ty.i1 as usize * src_stride..];
        let wy = ty.w;
        let d = (dst_y + row) * dst_stride + dx;
        let out = &mut dst[d..d + row_len];
        for (px, tx) in out.as_chunks_mut::<C>().0.iter_mut().zip(&axes.x) {
            let (a, b, wx) = (tx.i0 as usize * C, tx.i1 as usize * C, tx.w);
            for c in 0..C {
                let top = u32::from(r0[a + c]) * (256 - wx) + u32::from(r0[b + c]) * wx;
                let bot = u32::from(r1[a + c]) * (256 - wx) + u32::from(r1[b + c]) * wx;
                px[c] = ((top * (256 - wy) + bot * wy + (1 << 15)) >> 16) as u8;
            }
        }
    }
}

/// YUV → RGB matrix.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Matrix {
    /// SD video.
    Bt601,
    /// HD video.
    Bt709,
}

impl Matrix {
    /// The usual guess when a stream doesn't say: BT.709 for 720p and up, BT.601 below.
    pub fn for_height(height: u32) -> Self {
        if height >= 720 {
            Self::Bt709
        } else {
            Self::Bt601
        }
    }
}

/// Converts an NV12 frame (limited range) to BGRA (alpha 255), `width * 4` bytes per row.
pub fn nv12_to_bgra(frame: &Frame, matrix: Matrix, out: &mut Vec<u8>) {
    // Coefficients * 256: (Y, V→R, U→G, V→G, U→B).
    let (ky, kvr, kug, kvg, kub) = match matrix {
        Matrix::Bt601 => (298, 409, 100, 208, 516),
        Matrix::Bt709 => (298, 459, 55, 136, 541),
    };
    let (w, h) = (frame.width() as usize, frame.height() as usize);
    out.resize(w * h * 4, 0);
    let (y_plane, uv_plane) = (frame.y(), frame.uv());
    for row in 0..h {
        let y_row = &y_plane[row * w..(row + 1) * w];
        let uv_row = &uv_plane[(row / 2) * w..(row / 2 + 1) * w];
        let out_row = &mut out[row * w * 4..(row + 1) * w * 4];
        for (x, px) in out_row.as_chunks_mut::<4>().0.iter_mut().enumerate() {
            let y = (i32::from(y_row[x]) - 16) * ky;
            let u = i32::from(uv_row[x & !1]) - 128;
            let v = i32::from(uv_row[(x & !1) + 1]) - 128;
            px[0] = clamp8((y + kub * u + 128) >> 8);
            px[1] = clamp8((y - kug * u - kvg * v + 128) >> 8);
            px[2] = clamp8((y + kvr * v + 128) >> 8);
            px[3] = 255;
        }
    }
}

fn clamp8(v: i32) -> u8 {
    v.clamp(0, 255) as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fit_letterbox() {
        // 4:3 into 16:9 → pillarbox.
        let (s, d) = fit(640, 480, 1280, 720, FitMode::Letterbox);
        assert_eq!(s, Rect::new(0, 0, 640, 480));
        assert_eq!(d, Rect::new(160, 0, 960, 720));
        // 16:9 into 4:3 → letterbox.
        let (_, d) = fit(1920, 1080, 640, 480, FitMode::Letterbox);
        assert_eq!(d, Rect::new(0, 60, 640, 360));
        // Same aspect → full.
        let (_, d) = fit(1920, 1080, 1280, 720, FitMode::Letterbox);
        assert_eq!(d, Rect::new(0, 0, 1280, 720));
    }

    #[test]
    fn fit_crop() {
        let (s, d) = fit(640, 480, 1280, 720, FitMode::Crop);
        assert_eq!(s, Rect::new(0, 60, 640, 360));
        assert_eq!(d, Rect::new(0, 0, 1280, 720));
        let (s, _) = fit(1920, 1080, 640, 480, FitMode::Crop);
        assert_eq!(s, Rect::new(240, 0, 1440, 1080));
    }

    #[test]
    fn fit_stretch_and_rects_are_even() {
        let (s, d) = fit(1000, 562, 640, 480, FitMode::Stretch);
        assert_eq!(s, Rect::new(0, 0, 1000, 562));
        assert_eq!(d, Rect::new(0, 0, 640, 480));
        for mode in [FitMode::Letterbox, FitMode::Crop] {
            for (sw, sh) in [(1000, 562), (1366, 768), (720, 576), (2, 2)] {
                let (s, d) = fit(sw, sh, 640, 480, mode);
                for v in [s.x, s.y, s.w, s.h, d.x, d.y, d.w, d.h] {
                    assert_eq!(v % 2, 0, "{mode:?} {sw}x{sh}: {s:?} {d:?}");
                }
                assert!(s.x + s.w <= sw && s.y + s.h <= sh);
                assert!(d.x + d.w <= 640 && d.y + d.h <= 480);
            }
        }
    }

    /// A frame with a constant Y and UV value.
    fn solid(w: u32, h: u32, y: u8, u: u8, v: u8) -> Frame {
        let mut data = vec![y; Frame::nv12_len(w, h)];
        for pair in data[(w * h) as usize..].as_chunks_mut::<2>().0.iter_mut() {
            pair[0] = u;
            pair[1] = v;
        }
        Frame::from_nv12(w, h, data)
    }

    #[test]
    fn solid_color_stays_solid() {
        let src = solid(1920, 1080, 81, 90, 240);
        let out = Scaler::new().scale(&src, 1280, 720, FitMode::Stretch);
        assert!(out.y().iter().all(|&v| v == 81));
        assert!(out.uv().as_chunks::<2>().0.iter().all(|p| *p == [90, 240]));
    }

    #[test]
    fn letterbox_bars_are_black() {
        let src = solid(640, 480, 200, 50, 60);
        let out = Scaler::new().scale(&src, 1280, 720, FitMode::Letterbox);
        let w = 1280usize;
        // Left bar, picture, right bar on the first row.
        assert_eq!(out.y()[0], BLACK_Y);
        assert_eq!(out.y()[159], BLACK_Y);
        assert_eq!(out.y()[160], 200);
        assert_eq!(out.y()[1119], 200);
        assert_eq!(out.y()[1120], BLACK_Y);
        assert_eq!(&out.uv()[0..2], &[NEUTRAL_UV, NEUTRAL_UV]);
        assert_eq!(&out.uv()[160..162], &[50, 60]);
        assert_eq!(out.y()[719 * w + 640], 200);
    }

    #[test]
    fn same_size_is_an_exact_copy() {
        let mut src = solid(64, 32, 0, 0, 0);
        let len = src.y().len();
        let mut data = src.clone().into_data();
        for (i, b) in data[..len].iter_mut().enumerate() {
            *b = i as u8;
        }
        src = Frame::from_nv12(64, 32, data);
        let out = Scaler::new().scale(&src, 64, 32, FitMode::Letterbox);
        assert_eq!(out.data(), src.data());
    }

    #[test]
    fn horizontal_gradient_is_interpolated() {
        // 4 px wide: 0, 100, 200, 250 → upscaled to 8 px is monotonic and keeps the ends.
        let mut data = vec![128u8; Frame::nv12_len(4, 2)];
        data[..8].copy_from_slice(&[0, 100, 200, 250, 0, 100, 200, 250]);
        let src = Frame::from_nv12(4, 2, data);
        let out = Scaler::new().scale(&src, 8, 2, FitMode::Stretch);
        let row = &out.y()[..8];
        assert_eq!(row[0], 0);
        assert_eq!(row[7], 250);
        assert!(row.windows(2).all(|p| p[0] <= p[1]), "{row:?}");
    }

    #[test]
    fn scaler_reuses_plan_and_handles_geometry_change() {
        let mut scaler = Scaler::new();
        let a = scaler.scale(&solid(320, 240, 50, 128, 128), 640, 480, FitMode::Crop);
        let b = scaler.scale(&solid(1280, 720, 60, 128, 128), 640, 480, FitMode::Crop);
        assert!(a.y().iter().all(|&v| v == 50));
        assert!(b.y().iter().all(|&v| v == 60));
    }

    #[test]
    fn bgra_primaries() {
        let mut out = Vec::new();
        // Limited-range black, white, and BT.709 red (Y=63, U=102, V=240).
        nv12_to_bgra(&solid(2, 2, 16, 128, 128), Matrix::Bt709, &mut out);
        assert_eq!(&out[..4], &[0, 0, 0, 255]);
        nv12_to_bgra(&solid(2, 2, 235, 128, 128), Matrix::Bt709, &mut out);
        assert_eq!(&out[..4], &[255, 255, 255, 255]);
        nv12_to_bgra(&solid(2, 2, 63, 102, 240), Matrix::Bt709, &mut out);
        let (b, g, r) = (out[0], out[1], out[2]);
        assert!(r > 250 && g < 5 && b < 5, "{b} {g} {r}");
    }
}
