//! Safe Rust API over `MFCreateVirtualCamera` / `IMFVirtualCamera`.
//!
//! Cameras are created with session lifetime and current-user access: no admin rights are
//! needed, and they disappear when the [`VirtualCamera`] is dropped or the process exits.
//! The media source DLL must be registered (`DllRegisterServer`, once, as admin).
//!
//! All calls need COM initialized on the calling thread ([`MfThread::init`]). Run them on a
//! background thread, never the UI thread: creating a camera can take a second.

#![cfg(windows)]

use std::ffi::c_void;
use std::fmt;
use std::sync::OnceLock;

use rtspcam_core::constants::{
    CAMERA_ID_ATTRIBUTE, CAMERA_ID_PROPERTY_FMTID, CAMERA_ID_PROPERTY_PID,
    PREFERRED_FORMAT_ATTRIBUTE, vcam_source_clsid_string,
};
use uuid::Uuid;
use windows::Win32::Devices::DeviceAndDriverInstallation::{
    CM_Get_Device_Interface_PropertyW, CR_SUCCESS,
};
use windows::Win32::Devices::Properties::{DEVPROP_TYPE_STRING, DEVPROPTYPE};
use windows::Win32::Foundation::{DEVPROPKEY, E_NOTIMPL, RPC_E_CHANGED_MODE};
use windows::Win32::Media::MediaFoundation::{
    IMFActivate, IMFVirtualCamera, MF_DEVSOURCE_ATTRIBUTE_FRIENDLY_NAME,
    MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE, MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_GUID,
    MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_SYMBOLIC_LINK, MF_VERSION, MFCreateAttributes,
    MFEnumDeviceSources, MFSTARTUP_NOSOCKET, MFShutdown, MFStartup, MFVirtualCameraAccess,
    MFVirtualCameraAccess_CurrentUser, MFVirtualCameraLifetime, MFVirtualCameraLifetime_Session,
    MFVirtualCameraType, MFVirtualCameraType_SoftwareCameraSource,
};
use windows::Win32::System::Com::{
    COINIT_MULTITHREADED, CoInitializeEx, CoTaskMemFree, CoUninitialize,
};
use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
use windows_core::{GUID, HRESULT, HSTRING, Interface as _, PCWSTR, s, w};

/// Errors from creating or controlling cameras.
#[derive(Debug, thiserror::Error)]
pub enum VcamError {
    #[error("virtual cameras need Windows 11 (MFCreateVirtualCamera is not available)")]
    Unsupported,
    #[error(
        "the RTSP Cam media source is not registered; run `rtspcam-cli vcam register` as administrator"
    )]
    NotRegistered,
    #[error(
        "Windows refused to create the camera. The media source DLL must be registered (run \
         tools/vcam/install-dev.ps1) and sit in a folder the Frame Server service can read \
         (such as C:\\Program Files\\RtspCam)"
    )]
    AccessDenied,
    #[error("{0}")]
    Windows(#[from] windows_core::Error),
}

impl VcamError {
    fn classify(e: windows_core::Error) -> Self {
        const REGDB_E_CLASSNOTREG: HRESULT = HRESULT(0x8004_0154_u32 as i32);
        const E_ACCESSDENIED: HRESULT = HRESULT(0x8007_0005_u32 as i32);
        match e.code() {
            REGDB_E_CLASSNOTREG => Self::NotRegistered,
            E_ACCESSDENIED => Self::AccessDenied,
            _ => Self::Windows(e),
        }
    }
}

/// COM (multithreaded) and Media Foundation initialized on this thread while alive.
#[derive(Debug)]
pub struct MfThread {
    uninit_com: bool,
}

impl MfThread {
    pub fn init() -> Result<Self, VcamError> {
        // SAFETY: COM initialization for the current thread, balanced in Drop.
        let hr = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        if hr.is_err() && hr != RPC_E_CHANGED_MODE {
            return Err(windows_core::Error::from(hr).into());
        }
        // SAFETY: balanced by MFShutdown in Drop.
        unsafe { MFStartup(MF_VERSION, MFSTARTUP_NOSOCKET)? };
        Ok(Self {
            uninit_com: hr.is_ok(),
        })
    }
}

impl Drop for MfThread {
    fn drop(&mut self) {
        // SAFETY: balances `init` on the same thread.
        unsafe {
            let _ = MFShutdown();
            if self.uninit_com {
                CoUninitialize();
            }
        }
    }
}

type CreateFn = unsafe extern "system" fn(
    MFVirtualCameraType,
    MFVirtualCameraLifetime,
    MFVirtualCameraAccess,
    PCWSTR,
    PCWSTR,
    *const GUID,
    u32,
    *mut *mut c_void,
) -> HRESULT;

/// `MFCreateVirtualCamera`, resolved at run time so the program still starts (and can say
/// what's wrong) on Windows 10.
fn create_fn() -> Option<CreateFn> {
    static FN: OnceLock<Option<usize>> = OnceLock::new();
    let addr = *FN.get_or_init(|| {
        // SAFETY: loading a system DLL by name and looking up an export.
        unsafe {
            let module = LoadLibraryW(w!("mfsensorgroup.dll")).ok()?;
            GetProcAddress(module, s!("MFCreateVirtualCamera")).map(|f| f as usize)
        }
    });
    // SAFETY: the export has exactly this signature (mfvirtualcamera.h).
    addr.map(|a| unsafe { std::mem::transmute::<usize, CreateFn>(a) })
}

/// Whether this Windows supports virtual cameras.
pub fn is_supported() -> bool {
    create_fn().is_some()
}

fn property_key(pid: u32) -> DEVPROPKEY {
    DEVPROPKEY {
        fmtid: GUID::from_u128(CAMERA_ID_PROPERTY_FMTID.as_u128()),
        pid,
    }
}

/// One virtual camera. Shut down (removed from every app) when dropped.
pub struct VirtualCamera {
    camera: IMFVirtualCamera,
    name: String,
    id: Uuid,
}

impl fmt::Debug for VirtualCamera {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("VirtualCamera")
            .field("name", &self.name)
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}

impl VirtualCamera {
    /// Creates and starts a camera called `name` (Windows adds " – Windows Virtual Camera" in
    /// some places) whose media source serves stream `id`. `preferred` (width, height, fps) is
    /// listed first among its formats.
    pub fn create(
        name: &str,
        id: Uuid,
        preferred: Option<(u32, u32, u32)>,
    ) -> Result<Self, VcamError> {
        let create = create_fn().ok_or(VcamError::Unsupported)?;
        let name_w = HSTRING::from(name);
        let clsid = HSTRING::from(vcam_source_clsid_string());
        let mut raw = std::ptr::null_mut();
        // SAFETY: valid strings for the call; on success `raw` is an owned IMFVirtualCamera.
        let camera = unsafe {
            create(
                MFVirtualCameraType_SoftwareCameraSource,
                MFVirtualCameraLifetime_Session,
                MFVirtualCameraAccess_CurrentUser,
                PCWSTR(name_w.as_ptr()),
                PCWSTR(clsid.as_ptr()),
                std::ptr::null(),
                0,
                &mut raw,
            )
            .ok()
            .map_err(|e| VcamError::classify(step("MFCreateVirtualCamera", e)))?;
            IMFVirtualCamera::from_raw(raw)
        };
        // SAFETY: valid camera; string properties are null-terminated UTF-16.
        unsafe {
            // Attributes on the camera are handed to the media source with its activation object.
            // (`AddProperty` and `AddRegistryEntry` are refused for a current-user camera.)
            camera.SetString(
                &GUID::from_u128(CAMERA_ID_ATTRIBUTE.as_u128()),
                &HSTRING::from(id.to_string()),
            )?;
            if let Some((w, h, fps)) = preferred {
                camera.SetString(
                    &GUID::from_u128(PREFERRED_FORMAT_ATTRIBUTE.as_u128()),
                    &HSTRING::from(format!("{w}x{h}@{fps}")),
                )?;
            }
            camera
                .Start(None)
                .map_err(|e| VcamError::classify(step("Start", e)))?;
        }
        Ok(Self {
            camera,
            name: name.to_owned(),
            id,
        })
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn id(&self) -> Uuid {
        self.id
    }

    /// Stops the camera (it stays registered until shut down).
    pub fn stop(&self) -> Result<(), VcamError> {
        // SAFETY: valid camera.
        unsafe { self.camera.Stop()? };
        Ok(())
    }

    /// Removes the camera from the system now.
    pub fn remove(self) -> Result<(), VcamError> {
        // SAFETY: valid camera; Drop then shuts it down.
        unsafe { self.camera.Remove()? };
        Ok(())
    }
}

impl Drop for VirtualCamera {
    fn drop(&mut self) {
        // SAFETY: valid camera; shutting down a removed camera is harmless.
        let _ = unsafe { self.camera.Shutdown() };
    }
}

/// A video capture device as Media Foundation lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaptureDevice {
    pub name: String,
    pub symbolic_link: String,
    /// The stream id, if it is one of our virtual cameras.
    pub rtspcam_id: Option<Uuid>,
}

/// Lists video capture devices (including virtual cameras).
pub fn list_devices() -> Result<Vec<CaptureDevice>, VcamError> {
    let mut attrs = None;
    let mut activates: *mut Option<IMFActivate> = std::ptr::null_mut();
    let mut count = 0u32;
    // SAFETY: MFEnumDeviceSources returns a CoTaskMemAlloc'ed array of `count` references,
    // each taken out exactly once before the array is freed.
    let list: Vec<IMFActivate> = unsafe {
        MFCreateAttributes(&mut attrs, 1)?;
        let attrs = attrs.ok_or_else(|| windows_core::Error::from(E_NOTIMPL))?;
        attrs.SetGUID(
            &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE,
            &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_GUID,
        )?;
        MFEnumDeviceSources(&attrs, &mut activates, &mut count)?;
        let list = if activates.is_null() {
            Vec::new()
        } else {
            std::slice::from_raw_parts_mut(activates, count as usize)
                .iter_mut()
                .filter_map(Option::take)
                .collect()
        };
        CoTaskMemFree(Some(activates as *const _));
        list
    };
    Ok(list
        .iter()
        .map(|a| {
            let symbolic_link = string(a, &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_SYMBOLIC_LINK);
            CaptureDevice {
                name: string(a, &MF_DEVSOURCE_ATTRIBUTE_FRIENDLY_NAME),
                rtspcam_id: camera_id(&symbolic_link),
                symbolic_link,
            }
        })
        .collect())
}

fn string(a: &IMFActivate, key: &GUID) -> String {
    let mut ptr = windows_core::PWSTR::null();
    let mut len = 0;
    // SAFETY: GetAllocatedString returns a CoTaskMemAlloc'ed string we free.
    unsafe {
        if a.GetAllocatedString(key, &mut ptr, &mut len).is_err() {
            return String::new();
        }
        let s = String::from_utf16_lossy(std::slice::from_raw_parts(ptr.0, len as usize));
        CoTaskMemFree(Some(ptr.0 as *const _));
        s
    }
}

fn camera_id(symbolic_link: &str) -> Option<Uuid> {
    let link = HSTRING::from(symbolic_link);
    let key = property_key(CAMERA_ID_PROPERTY_PID);
    let mut ty = DEVPROPTYPE::default();
    let mut buf = [0u16; 64];
    let mut size = (buf.len() * 2) as u32;
    // SAFETY: valid string and buffer of `size` bytes.
    let cr = unsafe {
        CM_Get_Device_Interface_PropertyW(
            PCWSTR(link.as_ptr()),
            &key,
            &mut ty,
            Some(buf.as_mut_ptr().cast()),
            &mut size,
            0,
        )
    };
    if cr != CR_SUCCESS || ty != DEVPROP_TYPE_STRING {
        return None;
    }
    let s = String::from_utf16_lossy(&buf[..(size as usize / 2)]);
    Uuid::parse_str(s.trim_end_matches('\0')).ok()
}

/// Names the failing call in an error, so "Access is denied" says where.
fn step(call: &str, e: windows_core::Error) -> windows_core::Error {
    windows_core::Error::new(e.code(), format!("{call}: {}", e.message()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_devices_without_error() {
        let _mf = MfThread::init().unwrap();
        let devices = list_devices().unwrap();
        for d in &devices {
            assert!(!d.symbolic_link.is_empty() || d.name.is_empty());
        }
    }
}
