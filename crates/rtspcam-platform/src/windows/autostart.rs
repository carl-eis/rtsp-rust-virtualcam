//! "Start with Windows": a value in `HKCU\Software\Microsoft\Windows\CurrentVersion\Run`.
//! Per user, so no administrator rights are needed.

use std::io;
use std::path::Path;

use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS};
use windows::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, KEY_SET_VALUE, REG_SZ, RRF_RT_REG_SZ, RegCloseKey, RegDeleteValueW,
    RegGetValueW, RegOpenKeyExW, RegSetValueExW,
};
use windows::core::{HSTRING, PCWSTR, w};

use crate::autostart::{Autostart, MINIMIZED_ARG};

const RUN_KEY: PCWSTR = w!(r"Software\Microsoft\Windows\CurrentVersion\Run");
/// The value name used by the app (the installer's autostart task writes the same one).
pub(crate) const VALUE_NAME: &str = "RtspCam";

/// The app's entry in the `Run` key.
#[derive(Debug, Clone)]
pub(crate) struct RunKey {
    name: String,
}

impl Default for RunKey {
    fn default() -> Self {
        Self {
            name: VALUE_NAME.to_owned(),
        }
    }
}

impl Autostart for RunKey {
    fn label(&self) -> &'static str {
        "Start with Windows"
    }

    fn is_enabled(&self) -> bool {
        get_named(&self.name).is_some()
    }

    fn set(&self, enabled: bool) -> io::Result<()> {
        let exe = std::env::current_exe()?;
        set_named(&self.name, enabled.then(|| command_line(&exe)).as_deref())
            .map_err(io::Error::other)
    }
}

/// The command line stored in the registry for `exe`.
fn command_line(exe: &Path) -> String {
    format!("\"{}\" {MINIMIZED_ARG}", exe.display())
}

/// Sets (`Some`) or removes (`None`) the Run value `name`.
fn set_named(name: &str, command: Option<&str>) -> windows::core::Result<()> {
    let name = HSTRING::from(name);
    let mut key = HKEY::default();
    // SAFETY: a valid key path and output handle; the key is closed below.
    unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, RUN_KEY, None, KEY_SET_VALUE, &mut key) }.ok()?;
    let result = match command {
        Some(command) => {
            let data: Vec<u8> = command
                .encode_utf16()
                .chain(std::iter::once(0))
                .flat_map(u16::to_le_bytes)
                .collect();
            // SAFETY: `key` is open for writing and `data` is a null-terminated UTF-16 string.
            unsafe { RegSetValueExW(key, &name, None, REG_SZ, Some(&data)) }.ok()
        }
        None => {
            // SAFETY: `key` is open for writing.
            let err = unsafe { RegDeleteValueW(key, &name) };
            // Already gone is fine.
            if err == ERROR_FILE_NOT_FOUND {
                Ok(())
            } else {
                err.ok()
            }
        }
    };
    // SAFETY: closes the key opened above.
    unsafe {
        let _ = RegCloseKey(key);
    }
    result
}

/// The command stored in the Run value `name`, if any.
fn get_named(name: &str) -> Option<String> {
    let name = HSTRING::from(name);
    let mut buf = vec![0u16; 1024];
    let mut size = (buf.len() * 2) as u32;
    // SAFETY: valid key path, value name and a buffer of `size` bytes.
    let err = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            RUN_KEY,
            &name,
            RRF_RT_REG_SZ,
            None,
            Some(buf.as_mut_ptr().cast()),
            Some(&mut size),
        )
    };
    if err != ERROR_SUCCESS {
        return None;
    }
    let len = (size as usize / 2).saturating_sub(1);
    Some(String::from_utf16_lossy(&buf[..len]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_line_quotes_the_path() {
        let cmd = command_line(Path::new(r"C:\Program Files\RtspCam\rtspcam.exe"));
        assert_eq!(cmd, r#""C:\Program Files\RtspCam\rtspcam.exe" --minimized"#);
    }

    #[test]
    fn set_read_and_remove() {
        // A throwaway value name, so a developer's real autostart entry is never touched.
        let name = format!("RtspCamTest{}", std::process::id());
        assert_eq!(get_named(&name), None);
        set_named(&name, Some(r#""C:\x y\a.exe" --minimized"#)).unwrap();
        assert_eq!(
            get_named(&name).as_deref(),
            Some(r#""C:\x y\a.exe" --minimized"#)
        );
        set_named(&name, None).unwrap();
        assert_eq!(get_named(&name), None);
        // Removing twice is fine.
        set_named(&name, None).unwrap();
    }

    #[test]
    fn the_trait_writes_this_exe() {
        let entry = RunKey {
            name: format!("RtspCamTrait{}", std::process::id()),
        };
        assert!(!entry.is_enabled());
        entry.set(true).unwrap();
        let exe = std::env::current_exe().unwrap();
        assert_eq!(get_named(&entry.name), Some(command_line(&exe)));
        assert!(entry.is_enabled());
        entry.set(false).unwrap();
        assert!(!entry.is_enabled());
    }
}
