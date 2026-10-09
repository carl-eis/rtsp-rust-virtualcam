//! The preview pane: a child window that paints the newest picture (or a message) with GDI.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use rtspcam_core::FitMode;
use rtspcam_pipeline::scale::nv12_to_bgra;
use rtspcam_pipeline::{Frame, Matrix, Scaler};
use windows::Win32::Foundation::{COLORREF, HWND, RECT};
use windows::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BeginPaint, CreateSolidBrush, DIB_RGB_COLORS, DT_CENTER,
    DT_WORDBREAK, DeleteObject, DrawTextW, EndPaint, FillRect, InvalidateRect, PAINTSTRUCT,
    SRCCOPY, SetBkMode, SetTextColor, StretchDIBits, TRANSPARENT,
};
use windows::Win32::UI::WindowsAndMessaging::GetClientRect;
use winsafe::{self as w, co, gui, prelude::*};

const BACKGROUND: COLORREF = COLORREF(0x0018_1818);
const TEXT: COLORREF = COLORREF(0x00d8_d8d8);

#[derive(Default)]
struct Content {
    /// The last picture, already scaled to the window and converted to BGRA.
    bgra: Vec<u8>,
    size: (i32, i32),
    /// Shown instead of a picture.
    message: String,
    scaler: Scaler,
}

/// See the [module docs](self).
#[derive(Clone)]
pub(crate) struct PreviewPane {
    wnd: gui::WindowControl,
    content: Rc<RefCell<Content>>,
}

impl PreviewPane {
    pub(crate) fn new(
        parent: &(impl GuiParent + 'static),
        pos: (i32, i32),
        size: (i32, i32),
    ) -> Self {
        let wnd = gui::WindowControl::new(
            parent,
            gui::WindowControlOpts {
                position: pos,
                size,
                class_bg_brush: gui::Brush::Color(co::COLOR::BACKGROUND),
                ..Default::default()
            },
        );
        let me = Self {
            wnd,
            content: Rc::default(),
        };
        me.events();
        me
    }

    fn events(&self) {
        self.wnd.on().wm_erase_bkgnd(|_| Ok(1));
        let me = self.clone();
        self.wnd.on().wm_paint(move || {
            me.paint();
            Ok(())
        });
    }

    fn hwnd(&self) -> HWND {
        HWND(self.wnd.hwnd().ptr())
    }

    /// Shows `frame` (letterboxed to the pane), or `message` when there is none.
    pub(crate) fn show(&self, frame: Option<&Arc<Frame>>, message: &str) {
        let mut c = self.content.borrow_mut();
        let mut rect = RECT::default();
        // SAFETY: a valid window handle and RECT.
        if unsafe { GetClientRect(self.hwnd(), &mut rect) }.is_err() {
            return;
        }
        let (w, h) = (rect.right.max(2) & !1, rect.bottom.max(2) & !1);
        match frame {
            Some(frame) => {
                let scaled = c
                    .scaler
                    .scale(frame, w as u32, h as u32, FitMode::Letterbox);
                let mut bgra = std::mem::take(&mut c.bgra);
                nv12_to_bgra(&scaled, Matrix::for_height(frame.height()), &mut bgra);
                c.bgra = bgra;
                c.size = (w, h);
                c.message.clear();
            }
            None => {
                c.bgra.clear();
                c.message = message.to_owned();
            }
        }
        drop(c);
        // SAFETY: a valid window handle; repaints the whole pane.
        unsafe {
            let _ = InvalidateRect(Some(self.hwnd()), None, false);
        }
    }

    fn paint(&self) {
        let c = self.content.borrow();
        let hwnd = self.hwnd();
        let mut ps = PAINTSTRUCT::default();
        // SAFETY: standard WM_PAINT sequence on our own window; every GDI object created here is
        // deleted before EndPaint, and the pixel buffer outlives the StretchDIBits call.
        unsafe {
            let hdc = BeginPaint(hwnd, &mut ps);
            let mut rect = RECT::default();
            let _ = GetClientRect(hwnd, &mut rect);
            let (w, h) = c.size;
            if !c.bgra.is_empty() && c.bgra.len() == (w * h * 4) as usize {
                let info = BITMAPINFO {
                    bmiHeader: BITMAPINFOHEADER {
                        biSize: size_of::<BITMAPINFOHEADER>() as u32,
                        biWidth: w,
                        // Negative: the rows are top-down.
                        biHeight: -h,
                        biPlanes: 1,
                        biBitCount: 32,
                        biCompression: BI_RGB.0,
                        ..Default::default()
                    },
                    ..Default::default()
                };
                StretchDIBits(
                    hdc,
                    0,
                    0,
                    rect.right,
                    rect.bottom,
                    0,
                    0,
                    w,
                    h,
                    Some(c.bgra.as_ptr().cast()),
                    &info,
                    DIB_RGB_COLORS,
                    SRCCOPY,
                );
            } else {
                let brush = CreateSolidBrush(BACKGROUND);
                FillRect(hdc, &rect, brush);
                let _ = DeleteObject(brush.into());
                SetBkMode(hdc, TRANSPARENT);
                SetTextColor(hdc, TEXT);
                let mut text: Vec<u16> = c.message.encode_utf16().collect();
                // Centered with some padding.
                let mut inner = RECT {
                    left: rect.left + 12,
                    top: rect.top + 12,
                    right: rect.right - 12,
                    bottom: rect.bottom - 12,
                };
                if !text.is_empty() {
                    DrawTextW(hdc, &mut text, &mut inner, DT_CENTER | DT_WORDBREAK);
                }
            }
            let _ = EndPaint(hwnd, &ps);
        }
    }
}

impl AsRef<gui::WindowControl> for PreviewPane {
    fn as_ref(&self) -> &gui::WindowControl {
        &self.wnd
    }
}

#[allow(dead_code)]
fn _assert_hwnd_type(h: &w::HWND) -> *mut std::ffi::c_void {
    h.ptr()
}
