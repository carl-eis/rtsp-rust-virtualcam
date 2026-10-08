//! `rtspcam_vcam.dll`: the Media Foundation custom media source behind every RTSP Cam virtual
//! camera.
//!
//! Windows' Frame Server service loads it (`CoCreateInstance` of
//! [`VCAM_SOURCE_CLSID`](rtspcam_core::constants::VCAM_SOURCE_CLSID)), gets an `IMFActivate`,
//! and activates the media source. The source receives frames from `rtspcam.exe` over a named
//! pipe and shows a placeholder picture when there are none.
//!
//! - [`activate`]: the activation object (`IMFActivate` + its attribute store).
//! - `source` / `stream`: `IMFMediaSourceEx` and `IMFMediaStream2`.
//! - `feed`: the pipe client thread. `placeholder`: the "no signal" pictures.
//!
//! Every exported function and COM method catches panics and returns an `HRESULT`: a panic
//! here would take down the Frame Server and every camera on the system.

#![cfg(windows)]

pub mod activate;
mod camera_id;
mod feed;
mod formats;
pub mod guard;
mod log;
mod placeholder;
mod registry;
mod source;
mod stream;

use std::ffi::c_void;
use std::path::PathBuf;
use std::sync::OnceLock;

use rtspcam_core::constants::VCAM_SOURCE_CLSID;
use windows::Win32::Foundation::{
    CLASS_E_CLASSNOTAVAILABLE, CLASS_E_NOAGGREGATION, E_POINTER, HMODULE, S_FALSE, S_OK,
};
use windows::Win32::System::Com::{IClassFactory, IClassFactory_Impl};
use windows::Win32::System::LibraryLoader::GetModuleFileNameW;
use windows::Win32::System::SystemServices::DLL_PROCESS_ATTACH;
use windows_core::{BOOL, GUID, HRESULT, IUnknown, Interface as _, Ref, implement};

pub use activate::create_activate;
pub use source::RTSPCAM_ATTR_CAMERA_ID;

use crate::guard::{ModuleRef, guard, lock_server};
use crate::log::log;

/// The CLSID as a windows-rs GUID.
pub const CLSID: GUID = GUID::from_u128(VCAM_SOURCE_CLSID.as_u128());

/// This DLL's module handle, saved in `DllMain` (as an address, so it is `Send`).
static MODULE: OnceLock<usize> = OnceLock::new();

/// Full path of this DLL.
fn dll_path() -> Option<PathBuf> {
    let module = HMODULE(*MODULE.get()? as *mut c_void);
    let mut buf = vec![0u16; 1024];
    // SAFETY: valid module handle and buffer.
    let len = unsafe { GetModuleFileNameW(Some(module), &mut buf) } as usize;
    (len > 0 && len < buf.len()).then(|| PathBuf::from(String::from_utf16_lossy(&buf[..len])))
}

#[implement(IClassFactory)]
struct ClassFactory {
    _module: ModuleRef,
}

impl IClassFactory_Impl for ClassFactory_Impl {
    fn CreateInstance(
        &self,
        outer: Ref<IUnknown>,
        iid: *const GUID,
        out: *mut *mut c_void,
    ) -> windows_core::Result<()> {
        guard("ClassFactory::CreateInstance", || {
            if out.is_null() {
                return Err(E_POINTER.into());
            }
            // SAFETY: `out` is non-null; COM requires it to be cleared on failure.
            unsafe { *out = std::ptr::null_mut() };
            if outer.is_some() {
                return Err(CLASS_E_NOAGGREGATION.into());
            }
            let activate = create_activate()?;
            // SAFETY: QueryInterface contract.
            unsafe { activate.query(iid, out).ok() }
        })
    }

    fn LockServer(&self, lock: BOOL) -> windows_core::Result<()> {
        lock_server(lock.as_bool());
        Ok(())
    }
}

/// # Safety
/// Called by the loader.
#[unsafe(no_mangle)]
pub unsafe extern "system" fn DllMain(module: HMODULE, reason: u32, _: *mut c_void) -> BOOL {
    if reason == DLL_PROCESS_ATTACH {
        let _ = MODULE.set(module.0 as usize);
    }
    true.into()
}

/// # Safety
/// COM contract: valid pointers from `CoGetClassObject`.
#[unsafe(no_mangle)]
pub unsafe extern "system" fn DllGetClassObject(
    clsid: *const GUID,
    iid: *const GUID,
    out: *mut *mut c_void,
) -> HRESULT {
    let result = guard("DllGetClassObject", || {
        if clsid.is_null() || out.is_null() {
            return Err(E_POINTER.into());
        }
        // SAFETY: checked non-null above.
        unsafe { *out = std::ptr::null_mut() };
        // SAFETY: checked non-null above.
        if unsafe { *clsid } != CLSID {
            return Err(CLASS_E_CLASSNOTAVAILABLE.into());
        }
        let factory: IClassFactory = ClassFactory {
            _module: ModuleRef::new(),
        }
        .into();
        // SAFETY: QueryInterface contract.
        unsafe { factory.query(iid, out).ok() }
    });
    result.map_or_else(|e| e.code(), |()| S_OK)
}

#[unsafe(no_mangle)]
pub extern "system" fn DllCanUnloadNow() -> HRESULT {
    if guard::can_unload() { S_OK } else { S_FALSE }
}

#[unsafe(no_mangle)]
pub extern "system" fn DllRegisterServer() -> HRESULT {
    let result = guard("DllRegisterServer", || {
        let path = dll_path().ok_or(E_POINTER)?;
        registry::register(&path)?;
        log!("registered {}", path.display());
        Ok(())
    });
    result.map_or_else(|e| e.code(), |()| S_OK)
}

#[unsafe(no_mangle)]
pub extern "system" fn DllUnregisterServer() -> HRESULT {
    let result = guard("DllUnregisterServer", registry::unregister);
    result.map_or_else(|e| e.code(), |()| S_OK)
}
