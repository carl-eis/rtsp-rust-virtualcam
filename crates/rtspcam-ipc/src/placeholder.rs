//! The picture a camera shows when there are no frames: "RTSP Cam is not running",
//! "Connecting...", "No signal" and so on. Drawn with a built-in 5x7 font so the Windows DLL
//! needs no fonts, images or GDI. Shared by the DLL and the app's push backends
//! (`rtspcam_platform::push`), so every OS shows the same pictures.

use std::time::Duration;

use crate::protocol::{PixelFormat, StreamStatus, VideoFormat};

/// A frame older than this counts as "no signal" and the placeholder is shown instead.
pub const STALE_AFTER: Duration = Duration::from_secs(3);

/// Background and text brightness (full range, 0-255).
const BACKGROUND: u8 = 24;
const TITLE: u8 = 230;
const DETAIL: u8 = 150;

/// The title and detail lines for the app's stream status when there is no fresh frame.
/// `message` is the status message from [`FrameSource::status`](crate::FrameSource::status).
pub fn text_for_status(status: StreamStatus, message: &str) -> (&'static str, &str) {
    match status {
        StreamStatus::Connecting => ("Connecting...", message),
        StreamStatus::Disabled => ("Camera disabled", ""),
        StreamStatus::Error => ("No signal", message),
        StreamStatus::Streaming => ("No signal", ""),
    }
}

/// Renders `title` (large) and `detail` (smaller, may be empty) centered on a dark frame.
pub fn render(format: &VideoFormat, title: &str, detail: &str) -> Vec<u8> {
    let (w, h) = (format.width as usize, format.height as usize);
    let mut luma = vec![BACKGROUND; w * h];
    let title_scale = (h / 110).max(1);
    let detail_scale = (title_scale / 2).max(1);
    let title_h = 7 * title_scale;
    let detail_h = if detail.is_empty() {
        0
    } else {
        7 * detail_scale
    };
    let gap = if detail.is_empty() { 0 } else { title_h };
    let top = h.saturating_sub(title_h + gap + detail_h) / 2;
    draw_line(&mut luma, w, h, title, title_scale, top, TITLE);
    if !detail.is_empty() {
        draw_line(
            &mut luma,
            w,
            h,
            detail,
            detail_scale,
            top + title_h + gap,
            DETAIL,
        );
    }
    to_format(&luma, format)
}

/// Draws one centered line of text, cut off with "..." if it doesn't fit.
fn draw_line(luma: &mut [u8], w: usize, h: usize, text: &str, scale: usize, top: usize, value: u8) {
    let advance = 6 * scale;
    let max_chars = (w.saturating_sub(4 * scale)) / advance;
    let mut chars: Vec<char> = text.chars().map(|c| c.to_ascii_uppercase()).collect();
    if chars.len() > max_chars {
        chars.truncate(max_chars.saturating_sub(3));
        chars.extend("...".chars());
    }
    let width = chars.len() * advance;
    let left = w.saturating_sub(width) / 2;
    for (i, c) in chars.iter().enumerate() {
        let rows = glyph(*c);
        for (gy, bits) in rows.iter().enumerate() {
            for gx in 0..5 {
                if bits & (0x10 >> gx) == 0 {
                    continue;
                }
                let x0 = left + i * advance + gx * scale;
                let y0 = top + gy * scale;
                for y in y0..(y0 + scale).min(h) {
                    let row = &mut luma[y * w..(y + 1) * w];
                    for px in row.iter_mut().take((x0 + scale).min(w)).skip(x0) {
                        *px = value;
                    }
                }
            }
        }
    }
}

/// Converts a full-range gray image to the camera's pixel format.
fn to_format(luma: &[u8], format: &VideoFormat) -> Vec<u8> {
    // Full range → video range (16-235).
    let video = |l: u8| 16 + ((u32::from(l) * 219 + 127) / 255) as u8;
    match format.pixel_format {
        // Gray: both 4:2:0 layouts are the Y plane followed by neutral chroma.
        PixelFormat::Nv12 | PixelFormat::I420 => {
            let mut out = Vec::with_capacity(format.frame_len());
            out.extend(luma.iter().map(|&l| video(l)));
            out.resize(format.frame_len(), 128);
            out
        }
        PixelFormat::Yuyv => luma.iter().flat_map(|&l| [video(l), 128]).collect(),
        PixelFormat::Rgb32 => luma.iter().flat_map(|&l| [l, l, l, 255]).collect(),
    }
}

/// 5x7 glyphs, one byte per row, bit 4 = leftmost pixel.
fn glyph(c: char) -> [u8; 7] {
    match c {
        'A' => [0x0E, 0x11, 0x11, 0x1F, 0x11, 0x11, 0x11],
        'B' => [0x1E, 0x11, 0x11, 0x1E, 0x11, 0x11, 0x1E],
        'C' => [0x0E, 0x11, 0x10, 0x10, 0x10, 0x11, 0x0E],
        'D' => [0x1E, 0x11, 0x11, 0x11, 0x11, 0x11, 0x1E],
        'E' => [0x1F, 0x10, 0x10, 0x1E, 0x10, 0x10, 0x1F],
        'F' => [0x1F, 0x10, 0x10, 0x1E, 0x10, 0x10, 0x10],
        'G' => [0x0E, 0x11, 0x10, 0x17, 0x11, 0x11, 0x0F],
        'H' => [0x11, 0x11, 0x11, 0x1F, 0x11, 0x11, 0x11],
        'I' => [0x0E, 0x04, 0x04, 0x04, 0x04, 0x04, 0x0E],
        'J' => [0x07, 0x02, 0x02, 0x02, 0x02, 0x12, 0x0C],
        'K' => [0x11, 0x12, 0x14, 0x18, 0x14, 0x12, 0x11],
        'L' => [0x10, 0x10, 0x10, 0x10, 0x10, 0x10, 0x1F],
        'M' => [0x11, 0x1B, 0x15, 0x15, 0x11, 0x11, 0x11],
        'N' => [0x11, 0x11, 0x19, 0x15, 0x13, 0x11, 0x11],
        'O' => [0x0E, 0x11, 0x11, 0x11, 0x11, 0x11, 0x0E],
        'P' => [0x1E, 0x11, 0x11, 0x1E, 0x10, 0x10, 0x10],
        'Q' => [0x0E, 0x11, 0x11, 0x11, 0x15, 0x12, 0x0D],
        'R' => [0x1E, 0x11, 0x11, 0x1E, 0x14, 0x12, 0x11],
        'S' => [0x0F, 0x10, 0x10, 0x0E, 0x01, 0x01, 0x1E],
        'T' => [0x1F, 0x04, 0x04, 0x04, 0x04, 0x04, 0x04],
        'U' => [0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x0E],
        'V' => [0x11, 0x11, 0x11, 0x11, 0x11, 0x0A, 0x04],
        'W' => [0x11, 0x11, 0x11, 0x15, 0x15, 0x15, 0x0A],
        'X' => [0x11, 0x11, 0x0A, 0x04, 0x0A, 0x11, 0x11],
        'Y' => [0x11, 0x11, 0x11, 0x0A, 0x04, 0x04, 0x04],
        'Z' => [0x1F, 0x01, 0x02, 0x04, 0x08, 0x10, 0x1F],
        '0' => [0x0E, 0x11, 0x13, 0x15, 0x19, 0x11, 0x0E],
        '1' => [0x04, 0x0C, 0x04, 0x04, 0x04, 0x04, 0x0E],
        '2' => [0x0E, 0x11, 0x01, 0x02, 0x04, 0x08, 0x1F],
        '3' => [0x1F, 0x02, 0x04, 0x02, 0x01, 0x11, 0x0E],
        '4' => [0x02, 0x06, 0x0A, 0x12, 0x1F, 0x02, 0x02],
        '5' => [0x1F, 0x10, 0x1E, 0x01, 0x01, 0x11, 0x0E],
        '6' => [0x06, 0x08, 0x10, 0x1E, 0x11, 0x11, 0x0E],
        '7' => [0x1F, 0x01, 0x02, 0x04, 0x08, 0x08, 0x08],
        '8' => [0x0E, 0x11, 0x11, 0x0E, 0x11, 0x11, 0x0E],
        '9' => [0x0E, 0x11, 0x11, 0x0F, 0x01, 0x02, 0x0C],
        ' ' => [0; 7],
        '.' => [0, 0, 0, 0, 0, 0x0C, 0x0C],
        ',' => [0, 0, 0, 0, 0x0C, 0x04, 0x08],
        '-' => [0, 0, 0, 0x1F, 0, 0, 0],
        ':' => [0, 0x0C, 0x0C, 0, 0x0C, 0x0C, 0],
        '/' => [0, 0x01, 0x02, 0x04, 0x08, 0x10, 0],
        '(' => [0x02, 0x04, 0x08, 0x08, 0x08, 0x04, 0x02],
        ')' => [0x08, 0x04, 0x02, 0x02, 0x02, 0x04, 0x08],
        '\'' | '"' => [0x0C, 0x04, 0x08, 0, 0, 0, 0],
        '!' => [0x04, 0x04, 0x04, 0x04, 0x04, 0, 0x04],
        _ => [0x0E, 0x11, 0x01, 0x02, 0x04, 0, 0x04], // '?'
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fmt(pixel_format: PixelFormat) -> VideoFormat {
        VideoFormat {
            width: 640,
            height: 480,
            fps: 30,
            pixel_format,
        }
    }

    #[test]
    fn sizes_match_the_format() {
        for pf in [
            PixelFormat::Nv12,
            PixelFormat::Rgb32,
            PixelFormat::Yuyv,
            PixelFormat::I420,
        ] {
            let f = fmt(pf);
            assert_eq!(render(&f, "NO SIGNAL", "details").len(), f.frame_len());
        }
    }

    #[test]
    fn text_is_drawn_and_centered() {
        let f = fmt(PixelFormat::Nv12);
        let img = render(&f, "NO SIGNAL", "");
        let y = &img[..640 * 480];
        let bright: Vec<usize> = (0..y.len()).filter(|&i| y[i] > 200).collect();
        assert!(!bright.is_empty());
        let (min_x, max_x) = bright.iter().fold((usize::MAX, 0), |(a, b), &i| {
            (a.min(i % 640), b.max(i % 640))
        });
        // Roughly symmetric around the middle.
        assert!((min_x + max_x) / 2 > 300 && (min_x + max_x) / 2 < 340);
        // Background stays dark, chroma neutral.
        assert!(y[0] < 40);
        assert!(img[640 * 480..].iter().all(|&c| c == 128));
    }

    #[test]
    fn every_yuv_layout_has_the_same_picture() {
        let nv12 = render(&fmt(PixelFormat::Nv12), "NO SIGNAL", "details");
        let i420 = render(&fmt(PixelFormat::I420), "NO SIGNAL", "details");
        assert_eq!(nv12, i420);
        let yuyv = render(&fmt(PixelFormat::Yuyv), "NO SIGNAL", "details");
        let (luma, chroma): (Vec<u8>, Vec<u8>) =
            yuyv.as_chunks::<2>().0.iter().map(|p| (p[0], p[1])).unzip();
        assert_eq!(luma, nv12[..640 * 480]);
        assert!(chroma.iter().all(|&c| c == 128));
    }

    #[test]
    fn status_text_matches_the_dll() {
        assert_eq!(
            text_for_status(StreamStatus::Connecting, "retrying"),
            ("Connecting...", "retrying")
        );
        assert_eq!(
            text_for_status(StreamStatus::Error, "401 Unauthorized"),
            ("No signal", "401 Unauthorized")
        );
        assert_eq!(
            text_for_status(StreamStatus::Streaming, "ignored"),
            ("No signal", "")
        );
        assert_eq!(
            text_for_status(StreamStatus::Disabled, "ignored"),
            ("Camera disabled", "")
        );
    }

    #[test]
    fn long_text_is_truncated_not_overflowing() {
        let f = VideoFormat {
            width: 64,
            height: 48,
            fps: 15,
            pixel_format: PixelFormat::Rgb32,
        };
        let img = render(&f, &"X".repeat(200), &"Y".repeat(500));
        assert_eq!(img.len(), f.frame_len());
    }
}
