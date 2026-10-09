//! The app icon (drawn at run time, so no resource compiler is needed) and the tray icon.

use windows::Win32::Foundation::HWND;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Shell::{
    NIF_ICON, NIF_INFO, NIF_MESSAGE, NIF_TIP, NIIF_INFO, NIM_ADD, NIM_DELETE, NIM_MODIFY,
    NOTIFY_ICON_DATA_FLAGS, NOTIFYICONDATAW, Shell_NotifyIconW,
};
use windows::Win32::UI::WindowsAndMessaging::{CreateIcon, DestroyIcon, HICON};

/// Draws a 32x32 camera-lens icon: a dark blue tile with a white ring and a blue centre.
fn pixels(size: i32) -> Vec<u8> {
    let mut bgra = vec![0u8; (size * size * 4) as usize];
    let c = (size as f32 - 1.0) / 2.0;
    for y in 0..size {
        for x in 0..size {
            let (dx, dy) = (x as f32 - c, y as f32 - c);
            let d = (dx * dx + dy * dy).sqrt() / (size as f32 / 2.0);
            // (b, g, r, a)
            let px = if d > 0.97 {
                [0, 0, 0, 0]
            } else if d > 0.62 {
                [0xa8, 0x5a, 0x1c, 0xff] // tile
            } else if d > 0.44 {
                [0xff, 0xff, 0xff, 0xff] // ring
            } else if d > 0.16 {
                [0xd8, 0x8c, 0x2c, 0xff] // lens
            } else {
                [0xff, 0xf0, 0xd8, 0xff] // highlight
            };
            let i = ((y * size + x) * 4) as usize;
            bgra[i..i + 4].copy_from_slice(&px);
        }
    }
    bgra
}

/// An owned icon handle. Destroyed on drop.
#[derive(Debug)]
pub(crate) struct AppIcon(pub(crate) HICON);

impl AppIcon {
    pub(crate) fn create(size: i32) -> windows::core::Result<Self> {
        let xor = pixels(size);
        // A 32-bit icon takes its shape from the alpha channel; the AND mask is ignored.
        let and = vec![0u8; (size * size / 8) as usize];
        // SAFETY: the buffers match the stated dimensions and are copied by CreateIcon.
        let icon = unsafe {
            let module = GetModuleHandleW(None)?;
            CreateIcon(
                Some(module.into()),
                size,
                size,
                1,
                32,
                and.as_ptr(),
                xor.as_ptr(),
            )?
        };
        Ok(Self(icon))
    }
}

impl Drop for AppIcon {
    fn drop(&mut self) {
        // SAFETY: an icon created by CreateIcon, destroyed once.
        unsafe {
            let _ = DestroyIcon(self.0);
        }
    }
}

/// The notification-area icon. Removed on drop.
#[derive(Debug)]
pub(crate) struct Tray {
    hwnd: HWND,
    id: u32,
    icon: HICON,
    callback: u32,
    added: bool,
}

fn copy_wide<const N: usize>(dst: &mut [u16; N], s: &str) {
    let mut n = 0;
    for (i, u) in s.encode_utf16().take(N - 1).enumerate() {
        dst[i] = u;
        n = i + 1;
    }
    dst[n] = 0;
}

impl Tray {
    /// `callback` is the window message the shell sends for mouse events on the icon.
    pub(crate) fn new(hwnd: HWND, icon: HICON, callback: u32) -> Self {
        Self {
            hwnd,
            id: 1,
            icon,
            callback,
            added: false,
        }
    }

    fn data(&self, flags: NOTIFY_ICON_DATA_FLAGS) -> NOTIFYICONDATAW {
        NOTIFYICONDATAW {
            cbSize: size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: self.hwnd,
            uID: self.id,
            uFlags: flags,
            uCallbackMessage: self.callback,
            hIcon: self.icon,
            ..Default::default()
        }
    }

    /// Adds the icon (again, after Explorer restarts).
    pub(crate) fn add(&mut self, tip: &str) {
        let mut data = self.data(NIF_ICON | NIF_MESSAGE | NIF_TIP);
        copy_wide(&mut data.szTip, tip);
        // SAFETY: a fully initialized NOTIFYICONDATAW for our window.
        self.added = unsafe { Shell_NotifyIconW(NIM_ADD, &data) }.as_bool();
        if !self.added {
            tracing::warn!("could not add the tray icon");
        }
    }

    pub(crate) fn set_tip(&self, tip: &str) {
        if !self.added {
            return;
        }
        let mut data = self.data(NIF_TIP);
        copy_wide(&mut data.szTip, tip);
        // SAFETY: as in `add`.
        unsafe {
            let _ = Shell_NotifyIconW(NIM_MODIFY, &data);
        }
    }

    /// A balloon notification from the icon.
    pub(crate) fn balloon(&self, title: &str, text: &str) {
        if !self.added {
            return;
        }
        let mut data = self.data(NIF_INFO);
        copy_wide(&mut data.szInfoTitle, title);
        copy_wide(&mut data.szInfo, text);
        data.dwInfoFlags = NIIF_INFO;
        // SAFETY: as in `add`.
        unsafe {
            let _ = Shell_NotifyIconW(NIM_MODIFY, &data);
        }
    }

    pub(crate) fn remove(&mut self) {
        if self.added {
            let data = self.data(NOTIFY_ICON_DATA_FLAGS(0));
            // SAFETY: as in `add`.
            unsafe {
                let _ = Shell_NotifyIconW(NIM_DELETE, &data);
            }
            self.added = false;
        }
    }
}

impl Drop for Tray {
    fn drop(&mut self) {
        self.remove();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn icon_is_round_with_transparent_corners() {
        let px = pixels(32);
        let alpha = |x: usize, y: usize| px[(y * 32 + x) * 4 + 3];
        assert_eq!(alpha(0, 0), 0);
        assert_eq!(alpha(31, 31), 0);
        assert_eq!(alpha(16, 16), 0xff);
    }

    #[test]
    fn wide_copy_truncates_and_terminates() {
        let mut buf = [0xffffu16; 4];
        copy_wide(&mut buf, "abcdef");
        assert_eq!(buf, [b'a'.into(), b'b'.into(), b'c'.into(), 0]);
    }
}
