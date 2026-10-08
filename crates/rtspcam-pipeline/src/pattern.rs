//! An animated test pattern, for trying virtual cameras without an RTSP source.
//!
//! 75% color bars, a white bar moving across (so motion and dropped frames are visible), and a
//! strip at the bottom showing the frame number in binary (white = 1), most significant bit
//! on the left.

use crate::Frame;

/// BT.709 75% color bars in limited-range YUV: white, yellow, cyan, green, magenta, red, blue.
const BARS: [(u8, u8, u8); 7] = [
    (180, 128, 128),
    (168, 44, 136),
    (145, 147, 44),
    (133, 63, 52),
    (63, 193, 204),
    (51, 109, 212),
    (28, 212, 120),
];
const COUNTER_BITS: u32 = 24;

/// Frame number `n` of the pattern at `width` x `height` (both even).
pub fn test_pattern(width: u32, height: u32, n: u64) -> Frame {
    let (w, h) = (width as usize, height as usize);
    let mut data = vec![0u8; w * h * 3 / 2];
    let (y_plane, uv_plane) = data.split_at_mut(w * h);

    let strip = (h / 8).max(2) & !1;
    let bars_h = h - strip;
    let bar_w = (w / 60).max(2) & !1;
    let bar_x = ((n as usize * 8) % w) & !1;
    let bit_w = (w / COUNTER_BITS as usize).max(1);

    for row in 0..h {
        for col in 0..w {
            let (y, u, v) = if row < bars_h {
                if (bar_x..bar_x + bar_w).contains(&col) {
                    (235, 128, 128)
                } else {
                    BARS[(col * BARS.len() / w).min(BARS.len() - 1)]
                }
            } else {
                let bit = COUNTER_BITS as usize - 1 - (col / bit_w).min(COUNTER_BITS as usize - 1);
                if (n >> bit) & 1 == 1 {
                    (235, 128, 128)
                } else {
                    (16, 128, 128)
                }
            };
            y_plane[row * w + col] = y;
            if row % 2 == 0 && col % 2 == 0 {
                let i = (row / 2) * w + col;
                uv_plane[i] = u;
                uv_plane[i + 1] = v;
            }
        }
    }
    Frame::from_nv12(width, height, data)
}

/// Reads the frame number back from the bottom strip (the inverse of [`test_pattern`]).
pub fn read_counter(frame: &Frame) -> u64 {
    let (w, h) = (frame.width() as usize, frame.height() as usize);
    let bit_w = (w / COUNTER_BITS as usize).max(1);
    let row = h - (h / 16).max(1);
    (0..COUNTER_BITS as usize).fold(0, |acc, i| {
        let x = i * bit_w + bit_w / 2;
        (acc << 1) | u64::from(frame.y()[row * w + x] > 128)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Scaler;

    #[test]
    fn counter_round_trips_even_after_scaling() {
        for n in [0, 1, 255, 12345, (1 << 24) - 1] {
            let f = test_pattern(1280, 720, n);
            assert_eq!(read_counter(&f), n);
            let scaled = Scaler::new().scale(&f, 640, 480, rtspcam_core::FitMode::Stretch);
            assert_eq!(read_counter(&scaled), n);
        }
    }

    #[test]
    fn bar_moves() {
        let a = test_pattern(320, 240, 0);
        let b = test_pattern(320, 240, 1);
        assert_ne!(a.y()[..320], b.y()[..320]);
    }
}
