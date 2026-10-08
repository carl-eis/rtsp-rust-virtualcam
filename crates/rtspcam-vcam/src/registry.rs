//! `DllRegisterServer` / `DllUnregisterServer`: the media source's CLSID under
//! `HKLM\Software\Classes\CLSID` (per-user registration doesn't work: the DLL is loaded by
//! services running as other accounts), and the shared log folder.

use std::path::Path;

use rtspcam_core::constants::vcam_source_clsid_string;
use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS, HLOCAL, LocalFree};
use windows::Win32::Security::Authorization::{
    ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows::Win32::Security::{
    DACL_SECURITY_INFORMATION, PROTECTED_DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR,
    SetFileSecurityW,
};
use windows::Win32::System::Registry::{
    HKEY, HKEY_LOCAL_MACHINE, KEY_WRITE, REG_OPTION_NON_VOLATILE, REG_SZ, RegCloseKey,
    RegCreateKeyExW, RegDeleteTreeW, RegSetValueExW,
};
use windows_core::{HRESULT, HSTRING, PCWSTR, w};

const FRIENDLY_NAME: &str = "RTSP Cam Media Source";

fn clsid_key() -> String {
    format!(r"Software\Classes\CLSID\{}", vcam_source_clsid_string())
}

pub(crate) fn register(dll: &Path) -> windows_core::Result<()> {
    let key = create_key(HKEY_LOCAL_MACHINE, &clsid_key())?;
    let result = (|| {
        set_string(key, None, FRIENDLY_NAME)?;
        let inproc = create_key(key, "InprocServer32")?;
        let r = set_string(inproc, None, &dll.to_string_lossy())
            .and_then(|()| set_string(inproc, Some("ThreadingModel"), "Both"));
        // SAFETY: key opened above.
        unsafe {
            let _ = RegCloseKey(inproc);
        }
        r
    })();
    // SAFETY: key opened above.
    unsafe {
        let _ = RegCloseKey(key);
    }
    result?;
    // Logging is best effort; registration must not fail because of it.
    if let Err(e) = create_log_dir() {
        crate::log::log!("could not prepare the log folder: {e}");
    }
    Ok(())
}

pub(crate) fn unregister() -> windows_core::Result<()> {
    let path = HSTRING::from(clsid_key());
    // SAFETY: deleting our own key tree.
    let err = unsafe { RegDeleteTreeW(HKEY_LOCAL_MACHINE, PCWSTR(path.as_ptr())) };
    if err == ERROR_SUCCESS || err == ERROR_FILE_NOT_FOUND {
        Ok(())
    } else {
        Err(HRESULT::from_win32(err.0).into())
    }
}

fn create_key(parent: HKEY, sub: &str) -> windows_core::Result<HKEY> {
    let sub = HSTRING::from(sub);
    let mut key = HKEY::default();
    // SAFETY: valid parent key and out-param.
    let err = unsafe {
        RegCreateKeyExW(
            parent,
            PCWSTR(sub.as_ptr()),
            None,
            PCWSTR::null(),
            REG_OPTION_NON_VOLATILE,
            KEY_WRITE,
            None,
            &mut key,
            None,
        )
    };
    if err == ERROR_SUCCESS {
        Ok(key)
    } else {
        Err(HRESULT::from_win32(err.0).into())
    }
}

fn set_string(key: HKEY, name: Option<&str>, value: &str) -> windows_core::Result<()> {
    let name = name.map(HSTRING::from);
    let data: Vec<u8> = value
        .encode_utf16()
        .chain(std::iter::once(0))
        .flat_map(u16::to_le_bytes)
        .collect();
    // SAFETY: valid key; REG_SZ data is a null-terminated UTF-16 string.
    let err = unsafe {
        RegSetValueExW(
            key,
            name.as_ref().map_or(PCWSTR::null(), |n| PCWSTR(n.as_ptr())),
            None,
            REG_SZ,
            Some(&data),
        )
    };
    if err == ERROR_SUCCESS {
        Ok(())
    } else {
        Err(HRESULT::from_win32(err.0).into())
    }
}

/// `%ProgramData%\RtspCam\logs`, writable by LOCAL SERVICE (Frame Server) and SYSTEM (Frame
/// Server Monitor), readable by users.
fn create_log_dir() -> windows_core::Result<()> {
    let dir = crate::log::log_dir().ok_or(windows::Win32::Foundation::E_FAIL)?;
    std::fs::create_dir_all(&dir)
        .map_err(|e| windows_core::Error::new(windows::Win32::Foundation::E_FAIL, e.to_string()))?;
    // SYSTEM and Administrators: full; LOCAL SERVICE: modify; Users: read. Inherited by files.
    let sddl = w!("D:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)(A;OICI;0x1301bf;;;LS)(A;OICI;FR;;;BU)");
    let mut sd = PSECURITY_DESCRIPTOR::default();
    // SAFETY: valid SDDL; the descriptor is freed below.
    unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(sddl, SDDL_REVISION_1, &mut sd, None)?;
        let path = HSTRING::from(dir.as_os_str());
        let ok = SetFileSecurityW(
            PCWSTR(path.as_ptr()),
            DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
            sd,
        );
        LocalFree(Some(HLOCAL(sd.0)));
        ok.ok()
    }
}
