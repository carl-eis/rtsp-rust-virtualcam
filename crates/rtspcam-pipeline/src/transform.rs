//! Crop, rotate and flip for NV12 pictures.
//!
//! Applied to the decoded picture before it is scaled to the camera's output size, in this
//! order: crop, rotate clockwise, flip.

use rtspcam_core::config::{Picture, Rotation};

use crate::Frame;

/// The geometry part of `picture` applied to `src`, or `None` if it changes nothing.
///
/// Crop edges are rounded to even pixels (NV12 chroma is shared by 2x2 blocks). A crop that
/// would leave less than 2x2 pixels is ignored.
pub fn adjust(src: &Frame, picture: &Picture) -> Option<Frame> {
    let (x0, y0, cw, ch) = crop_rect(src.width(), src.height(), picture);
    let cropped = (cw, ch) != (src.width(), src.height());
    let moved =
        picture.rotate != Rotation::None || picture.flip_horizontal || picture.flip_vertical;
    if !cropped && !moved {
        return None;
    }

    let quarter = matches!(picture.rotate, Rotation::Cw90 | Rotation::Cw270);
    let (ow, oh) = if quarter { (ch, cw) } else { (cw, ch) };
    let mut data = vec![0u8; Frame::nv12_len(ow, oh)];
    let (dst_y, dst_uv) = data.split_at_mut(ow as usize * oh as usize);

    let map = Mapping {
        rotate: picture.rotate,
        flip_h: picture.flip_horizontal,
        flip_v: picture.flip_vertical,
    };
    let (src_w, src_h) = (src.width() as usize, src.height() as usize);

    // Luma: one byte per pixel, in the cropped picture's own coordinates.
    for oy in 0..oh as usize {
        for ox in 0..ow as usize {
            let (sx, sy) = map.source(ox, oy, ow as usize, oh as usize, cw as usize, ch as usize);
            dst_y[oy * ow as usize + ox] = src.y()[(y0 as usize + sy) * src_w + x0 as usize + sx];
        }
    }
    // Chroma: the same mapping on the half-size grid, two bytes (U, V) per sample.
    let (ocw, och) = (ow as usize / 2, oh as usize / 2);
    for oy in 0..och {
        for ox in 0..ocw {
            let (sx, sy) = map.source(ox, oy, ocw, och, cw as usize / 2, ch as usize / 2);
            let from = (y0 as usize / 2 + sy) * src_w + (x0 as usize / 2 + sx) * 2;
            let to = oy * ow as usize + ox * 2;
            dst_uv[to..to + 2].copy_from_slice(&src.uv()[from..from + 2]);
        }
    }
    debug_assert_eq!(src.uv().len(), src_w * src_h / 2);

    let mut out = Frame::from_nv12(ow, oh, data);
    out.pts = src.pts;
    out.received = src.received;
    out.decoded = src.decoded;
    Some(out)
}

/// `(x, y, width, height)` of the kept region, all even.
fn crop_rect(w: u32, h: u32, picture: &Picture) -> (u32, u32, u32, u32) {
    let c = picture.crop;
    let cut = |len: u32, percent: u8| (len * u32::from(percent) / 100) & !1;
    let (l, r) = (cut(w, c.left), cut(w, c.right));
    let (t, b) = (cut(h, c.top), cut(h, c.bottom));
    if l + r + 2 > w || t + b + 2 > h {
        return (0, 0, w, h);
    }
    (l, t, w - l - r, h - t - b)
}

struct Mapping {
    rotate: Rotation,
    flip_h: bool,
    flip_v: bool,
}

impl Mapping {
    /// Where output sample `(x, y)` of an `ow` x `oh` grid comes from in the `cw` x `ch` source
    /// grid.
    fn source(
        &self,
        x: usize,
        y: usize,
        ow: usize,
        oh: usize,
        cw: usize,
        ch: usize,
    ) -> (usize, usize) {
        let x = if self.flip_h { ow - 1 - x } else { x };
        let y = if self.flip_v { oh - 1 - y } else { y };
        match self.rotate {
            Rotation::None => (x, y),
            Rotation::Cw90 => (y, ch - 1 - x),
            Rotation::Cw180 => (cw - 1 - x, ch - 1 - y),
            Rotation::Cw270 => (cw - 1 - y, x),
        }
    }
}

#[cfg(test)]
mod tests {
    use rtspcam_core::config::Crop;

    use super::*;

    /// 4x2 picture: Y = 0..8 row by row, chroma samples (10,11) (12,13).
    fn sample() -> Frame {
        let mut data: Vec<u8> = (0..8).collect();
        data.extend([10, 11, 12, 13]);
        Frame::from_nv12(4, 2, data)
    }

    fn pic(f: impl FnOnce(&mut Picture)) -> Picture {
        let mut p = Picture::default();
        f(&mut p);
        p
    }

    #[test]
    fn nothing_to_do_is_none() {
        assert!(adjust(&sample(), &Picture::default()).is_none());
        // Only overlay / disconnect options: not geometry.
        assert!(adjust(&sample(), &pic(|p| p.show_name = true)).is_none());
    }

    #[test]
    fn flips() {
        let h = adjust(&sample(), &pic(|p| p.flip_horizontal = true)).unwrap();
        assert_eq!(h.y(), &[3, 2, 1, 0, 7, 6, 5, 4]);
        assert_eq!(h.uv(), &[12, 13, 10, 11]);
        let v = adjust(&sample(), &pic(|p| p.flip_vertical = true)).unwrap();
        assert_eq!(v.y(), &[4, 5, 6, 7, 0, 1, 2, 3]);
        assert_eq!(v.uv(), &[10, 11, 12, 13], "one chroma row: unchanged");
    }

    #[test]
    fn rotate_180_is_both_flips() {
        let r = adjust(&sample(), &pic(|p| p.rotate = Rotation::Cw180)).unwrap();
        let both = adjust(
            &sample(),
            &pic(|p| {
                p.flip_horizontal = true;
                p.flip_vertical = true;
            }),
        )
        .unwrap();
        assert_eq!(r.data(), both.data());
        assert_eq!(r.y(), &[7, 6, 5, 4, 3, 2, 1, 0]);
    }

    #[test]
    fn rotate_90_clockwise() {
        // 0 1 2 3        4 0
        // 4 5 6 7  ->    5 1
        //                6 2
        //                7 3
        let r = adjust(&sample(), &pic(|p| p.rotate = Rotation::Cw90)).unwrap();
        assert_eq!((r.width(), r.height()), (2, 4));
        assert_eq!(r.y(), &[4, 0, 5, 1, 6, 2, 7, 3]);
        // Chroma grid 2x1 -> 1x2: left sample on top.
        assert_eq!(r.uv(), &[10, 11, 12, 13]);
    }

    #[test]
    fn rotate_270_clockwise() {
        // 3 7
        // 2 6
        // 1 5
        // 0 4
        let r = adjust(&sample(), &pic(|p| p.rotate = Rotation::Cw270)).unwrap();
        assert_eq!((r.width(), r.height()), (2, 4));
        assert_eq!(r.y(), &[3, 7, 2, 6, 1, 5, 0, 4]);
        assert_eq!(r.uv(), &[12, 13, 10, 11]);
    }

    #[test]
    fn four_quarter_turns_are_the_identity() {
        let mut f = sample();
        for _ in 0..4 {
            f = adjust(&f, &pic(|p| p.rotate = Rotation::Cw90)).unwrap();
        }
        assert_eq!(f.data(), sample().data());
    }

    #[test]
    fn crop_keeps_even_edges() {
        let (w, h) = (8u32, 4u32);
        let mut data: Vec<u8> = Vec::new();
        data.extend((0..w * h).map(|v| v as u8));
        data.extend((0..w * h / 2).map(|v| 100 + v as u8));
        let f = Frame::from_nv12(w, h, data);
        // 25% of 8 = 2 px off the left; 50% of 4 = 2 px off the bottom.
        let c = adjust(
            &f,
            &pic(|p| {
                p.crop = Crop {
                    left: 25,
                    bottom: 50,
                    ..Crop::default()
                }
            }),
        )
        .unwrap();
        assert_eq!((c.width(), c.height()), (6, 2));
        assert_eq!(c.y(), &[2, 3, 4, 5, 6, 7, 10, 11, 12, 13, 14, 15]);
        assert_eq!(c.uv(), &[102, 103, 104, 105, 106, 107]);
    }

    #[test]
    fn odd_percentages_round_down_to_even_pixels() {
        let f = Frame::black(10, 10);
        let c = adjust(
            &f,
            &pic(|p| {
                p.crop = Crop {
                    right: 15, // 1.5 px -> 0
                    top: 25,   // 2.5 px -> 2
                    ..Crop::default()
                }
            }),
        )
        .unwrap();
        assert_eq!((c.width(), c.height()), (10, 8));
    }

    #[test]
    fn crop_with_rotation_and_a_crop_that_leaves_nothing() {
        let f = Frame::black(8, 4);
        let r = adjust(
            &f,
            &pic(|p| {
                p.rotate = Rotation::Cw90;
                p.crop = Crop {
                    left: 25,
                    ..Crop::default()
                };
            }),
        )
        .unwrap();
        assert_eq!((r.width(), r.height()), (4, 6));
        // A hopeless crop is ignored rather than producing an empty picture.
        let ignored = adjust(
            &f,
            &pic(|p| {
                p.crop = Crop {
                    left: 80,
                    right: 80,
                    ..Crop::default()
                }
            }),
        );
        assert!(ignored.is_none());
    }
}
