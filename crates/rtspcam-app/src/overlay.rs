//! Text drawn onto camera pictures (the stream's name and the time).
//!
//! The text is rendered once into a coverage mask with GDI (so any installed font works),
//! then blended into the NV12 picture: a dimmed box behind the text and white text on top.

use std::collections::HashMap;

use rtspcam_pipeline::Frame;
use windows::Win32::Foundation::COLORREF;
use windows::Win32::Foundation::SIZE;
use windows::Win32::Graphics::Gdi::{
    ANTIALIASED_QUALITY, BI_RGB, BITMAPINFO, BITMAPINFOHEADER, CLIP_DEFAULT_PRECIS,
    CreateCompatibleDC, CreateDIBSection, CreateFontW, DEFAULT_CHARSET, DIB_RGB_COLORS, DeleteDC,
    DeleteObject, FF_DONTCARE, FW_SEMIBOLD, GetTextExtentPoint32W, HGDIOBJ, OUT_DEFAULT_PRECIS,
    SelectObject, SetBkMode, SetTextColor, TRANSPARENT, TextOutW,
};
use windows::Win32::System::SystemInformation::GetLocalTime;
use windows::core::w;

/// Luma of white text and of black in limited-range video.
const WHITE_Y: u32 = 235;
const BLACK_Y: u32 = 16;

/// Text coverage, 0 (nothing) to 255 (solid), one byte per pixel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Mask {
    pub(crate) width: usize,
    pub(crate) height: usize,
    pub(crate) coverage: Vec<u8>,
}

/// Renders `text` at `px` pixels high. `None` if GDI fails or the text is empty.
pub(crate) fn render_mask(text: &str, px: i32) -> Option<Mask> {
    let wide: Vec<u16> = text.encode_utf16().collect();
    if wide.is_empty() {
        return None;
    }
    // SAFETY: plain GDI calls on a memory DC that is created and destroyed here. The DIB
    // section's pixel memory is owned by the bitmap, which outlives the slice read from it, and
    // every object selected into the DC is deselected before it is deleted.
    unsafe {
        let dc = CreateCompatibleDC(None);
        if dc.is_invalid() {
            return None;
        }
        let font = CreateFontW(
            -px,
            0,
            0,
            0,
            FW_SEMIBOLD.0 as i32,
            0,
            0,
            0,
            DEFAULT_CHARSET,
            OUT_DEFAULT_PRECIS,
            CLIP_DEFAULT_PRECIS,
            ANTIALIASED_QUALITY,
            FF_DONTCARE.0 as u32,
            w!("Segoe UI"),
        );
        let old_font = SelectObject(dc, HGDIOBJ(font.0));
        let mut size = SIZE::default();
        let measured = GetTextExtentPoint32W(dc, &wide, &mut size).as_bool();
        let (width, height) = (size.cx.max(0) as usize, size.cy.max(0) as usize);

        let result = if measured && width > 0 && height > 0 {
            let info = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: width as i32,
                    biHeight: -(height as i32), // top-down
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0,
                    ..Default::default()
                },
                ..Default::default()
            };
            let mut bits = std::ptr::null_mut();
            match CreateDIBSection(Some(dc), &info, DIB_RGB_COLORS, &mut bits, None, 0) {
                Ok(bitmap) if !bits.is_null() => {
                    let old_bitmap = SelectObject(dc, HGDIOBJ(bitmap.0));
                    // A new DIB section is black; white text on it gives the coverage directly.
                    SetBkMode(dc, TRANSPARENT);
                    SetTextColor(dc, COLORREF(0x00ff_ffff));
                    let _ = TextOutW(dc, 0, 0, &wide);
                    let pixels = std::slice::from_raw_parts(bits as *const u8, width * height * 4);
                    // Grayscale anti-aliasing: the green channel is the coverage.
                    let coverage = pixels.chunks_exact(4).map(|p| p[1]).collect();
                    SelectObject(dc, old_bitmap);
                    let _ = DeleteObject(HGDIOBJ(bitmap.0));
                    Some(Mask {
                        width,
                        height,
                        coverage,
                    })
                }
                _ => None,
            }
        } else {
            None
        };
        SelectObject(dc, old_font);
        let _ = DeleteObject(HGDIOBJ(font.0));
        let _ = DeleteDC(dc);
        result
    }
}

/// Darkens a box of the picture by half (luma only, so colours stay put).
fn dim(data: &mut [u8], pic_w: usize, x: usize, y: usize, w: usize, h: usize) {
    for row in y..y + h {
        for px in &mut data[row * pic_w + x..row * pic_w + x + w] {
            *px = (BLACK_Y + (u32::from(*px).saturating_sub(BLACK_Y)) / 2) as u8;
        }
    }
}

/// Blends white text with the given coverage into the picture at `(x, y)`.
fn blend(data: &mut [u8], pic_w: usize, pic_h: usize, mask: &Mask, x: usize, y: usize) {
    let (luma, chroma) = data.split_at_mut(pic_w * pic_h);
    for my in 0..mask.height {
        for mx in 0..mask.width {
            let a = u32::from(mask.coverage[my * mask.width + mx]);
            if a == 0 {
                continue;
            }
            let p = &mut luma[(y + my) * pic_w + x + mx];
            *p = (u32::from(*p) + (WHITE_Y.saturating_sub(u32::from(*p)) * a + 127) / 255) as u8;
        }
    }
    // Pull the colour of covered 2x2 blocks towards neutral so text isn't tinted.
    for cy in (y / 2)..((y + mask.height).div_ceil(2)).min(pic_h / 2) {
        for cx in (x / 2)..((x + mask.width).div_ceil(2)).min(pic_w / 2) {
            let (mx, my) = ((cx * 2).saturating_sub(x), (cy * 2).saturating_sub(y));
            let (mx, my) = (mx.min(mask.width - 1), my.min(mask.height - 1));
            let a = u32::from(mask.coverage[my * mask.width + mx]);
            if a == 0 {
                continue;
            }
            for c in &mut chroma[cy * pic_w + cx * 2..cy * pic_w + cx * 2 + 2] {
                let v = u32::from(*c);
                *c = if v < 128 {
                    v + ((128 - v) * a + 127) / 255
                } else {
                    v - ((v - 128) * a + 127) / 255
                } as u8;
            }
        }
    }
}

/// Draws and caches text masks, so a name is rendered once and the clock once a second.
#[derive(Debug, Default)]
pub(crate) struct Overlay {
    masks: HashMap<(String, i32), Option<Mask>>,
}

impl Overlay {
    /// Returns `frame` with `lines` drawn in its bottom-left corner, one above the other.
    pub(crate) fn draw(&mut self, frame: Frame, lines: &[String]) -> Frame {
        let (w, h) = (frame.width() as usize, frame.height() as usize);
        let px = (h / 28).clamp(14, 72) as i32;
        // Keep the cache from growing with every second of the clock.
        if self.masks.len() > 16 {
            self.masks.clear();
        }
        let masks: Vec<Mask> = lines
            .iter()
            .filter_map(|line| {
                self.masks
                    .entry((line.clone(), px))
                    .or_insert_with(|| render_mask(line, px))
                    .clone()
            })
            .collect();
        if masks.is_empty() {
            return frame;
        }

        let pad = (px as usize / 3).max(3) & !1;
        let box_w = masks.iter().map(|m| m.width).max().unwrap_or(0) + 2 * pad;
        let box_h = masks.iter().map(|m| m.height).sum::<usize>() + 2 * pad;
        if box_w + 2 * pad > w || box_h + 2 * pad > h {
            return frame; // the picture is too small to carry text
        }
        let (box_x, box_y) = (pad & !1, (h - box_h - pad) & !1);

        let (pts, received, decoded) = (frame.pts, frame.received, frame.decoded);
        let mut data = frame.into_data();
        dim(&mut data[..w * h], w, box_x, box_y, box_w, box_h);
        let mut y = box_y + pad;
        for mask in &masks {
            blend(&mut data, w, h, mask, box_x + pad, y);
            y += mask.height;
        }
        let mut out = Frame::from_nv12(w as u32, h as u32, data);
        out.pts = pts;
        out.received = received;
        out.decoded = decoded;
        out
    }
}

/// The local time as `2026-10-09 14:03:07`.
pub(crate) fn local_time_text() -> String {
    // SAFETY: GetLocalTime has no preconditions.
    let t = unsafe { GetLocalTime() };
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
        t.wYear, t.wMonth, t.wDay, t.wHour, t.wMinute, t.wSecond
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_renders_a_mask_with_ink() {
        let mask = render_mask("Front Door 12:30", 28).expect("GDI should render text");
        assert!(mask.width > 60 && mask.height >= 20, "{mask:?}");
        assert_eq!(mask.coverage.len(), mask.width * mask.height);
        let ink = mask.coverage.iter().filter(|&&c| c > 128).count();
        assert!(ink > 50, "only {ink} solid pixels");
        assert!(mask.coverage.contains(&0), "background stays empty");
        assert!(render_mask("", 28).is_none());
    }

    #[test]
    fn draws_a_bright_patch_and_dims_its_box_only() {
        let mut overlay = Overlay::default();
        let frame = Frame::from_nv12(640, 480, {
            let mut d = vec![128u8; Frame::nv12_len(640, 480)];
            d[..640 * 480].fill(180);
            d
        });
        let out = overlay.draw(
            frame,
            &["Front Door".to_owned(), "2026-10-09 14:03:07".to_owned()],
        );
        assert_eq!((out.width(), out.height()), (640, 480));
        let y = out.y();
        // The top of the picture is untouched.
        assert!(y[..640 * 100].iter().all(|&p| p == 180));
        // Somewhere near the bottom-left there is bright text and dimmed background.
        let corner: Vec<u8> = (380..480)
            .flat_map(|row| y[row * 640..row * 640 + 300].iter().copied())
            .collect();
        assert!(corner.iter().any(|&p| p > 225), "no text pixels");
        assert!(corner.iter().any(|&p| p < 120), "no dimmed box");
        // The right half of the bottom is untouched.
        assert!(
            y[470 * 640 + 400..470 * 640 + 640]
                .iter()
                .all(|&p| p == 180)
        );
    }

    #[test]
    fn tiny_pictures_are_left_alone() {
        let mut overlay = Overlay::default();
        let frame = Frame::black(32, 32);
        let out = overlay.draw(
            frame.clone(),
            &["A very long name for a tiny picture".to_owned()],
        );
        assert_eq!(out.data(), frame.data());
    }

    #[test]
    fn clock_text_has_the_expected_shape() {
        let t = local_time_text();
        assert_eq!(t.len(), 19, "{t}");
        assert_eq!(&t[4..5], "-");
        assert_eq!(&t[13..14], ":");
    }
}
