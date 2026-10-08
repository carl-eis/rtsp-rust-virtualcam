//! The activation object handed out by the class factory. Frame Server sets attributes on it,
//! then calls `ActivateObject` to get the media source.

use std::ffi::c_void;
use std::sync::Mutex;

use windows::Win32::Foundation::E_POINTER;
use windows::Win32::Media::MediaFoundation::{
    IMFActivate, IMFActivate_Impl, IMFAttributes, IMFAttributes_Impl, IMFMediaSource,
    MF_ATTRIBUTE_TYPE, MF_ATTRIBUTES_MATCH_TYPE, MFCreateAttributes,
};
use windows::Win32::System::Com::StructuredStorage::PROPVARIANT;
use windows_core::{BOOL, GUID, IUnknown, Interface, PCWSTR, PWSTR, Ref, implement};

use crate::guard::{ModuleRef, guard};
use crate::log::log;
use crate::source::MediaSource;

#[implement(IMFActivate)]
pub(crate) struct Activate {
    attributes: IMFAttributes,
    source: Mutex<Option<IMFMediaSource>>,
    _module: ModuleRef,
}

/// Creates the activation object (what `DllGetClassObject`'s factory returns).
pub fn create_activate() -> windows_core::Result<IMFActivate> {
    let mut attributes = None;
    // SAFETY: out-param for a new attribute store.
    unsafe { MFCreateAttributes(&mut attributes, 4)? };
    Ok(Activate {
        attributes: attributes.ok_or(E_POINTER)?,
        source: Mutex::new(None),
        _module: ModuleRef::new(),
    }
    .into())
}

impl IMFActivate_Impl for Activate_Impl {
    fn ActivateObject(&self, iid: *const GUID, out: *mut *mut c_void) -> windows_core::Result<()> {
        guard("Activate::ActivateObject", || {
            let mut slot = self.source.lock().unwrap_or_else(|e| e.into_inner());
            if slot.is_none() {
                crate::source::log_attribute_keys("ActivateObject", &self.attributes);
                let source = MediaSource::create(self.attributes.clone())
                    .inspect_err(|e| log!("could not create the media source: {e}"))?;
                *slot = Some(source);
            }
            let source = slot.as_ref().expect("source was just created");
            // SAFETY: COM QueryInterface contract; `iid` and `out` come from the caller.
            unsafe { source.query(iid, out).ok() }
        })
    }

    fn ShutdownObject(&self) -> windows_core::Result<()> {
        guard("Activate::ShutdownObject", || {
            let source = self.source.lock().unwrap_or_else(|e| e.into_inner()).take();
            if let Some(source) = source {
                // SAFETY: valid source.
                unsafe { source.Shutdown()? };
            }
            Ok(())
        })
    }

    fn DetachObject(&self) -> windows_core::Result<()> {
        guard("Activate::DetachObject", || {
            log!("DetachObject");
            self.source.lock().unwrap_or_else(|e| e.into_inner()).take();
            Ok(())
        })
    }
}

/// Calls the inner attribute store's vtable entry with the same raw arguments.
macro_rules! forward {
    ($self:ident . $method:ident ( $($arg:expr),* )) => {
        // SAFETY: forwards the caller's arguments unchanged to an IMFAttributes with the same
        // contract.
        guard(stringify!($method), || unsafe {
            (Interface::vtable(&$self.attributes).$method)(Interface::as_raw(&$self.attributes) $(, $arg)*).ok()
        })
    };
}

/// Like `forward!` for methods with one out-value.
macro_rules! forward_out {
    ($self:ident . $method:ident ( $($arg:expr),* ) -> $ty:ty) => {
        // SAFETY: as in `forward!`; `out` is a valid out-param of the right type.
        guard(stringify!($method), || unsafe {
            let mut out = <$ty>::default();
            (Interface::vtable(&$self.attributes).$method)(Interface::as_raw(&$self.attributes) $(, $arg)*, &mut out).ok()?;
            Ok(out)
        })
    };
}

impl IMFAttributes_Impl for Activate_Impl {
    fn GetItem(&self, key: *const GUID, value: *mut PROPVARIANT) -> windows_core::Result<()> {
        forward!(self.GetItem(key, value.cast()))
    }
    fn GetItemType(&self, key: *const GUID) -> windows_core::Result<MF_ATTRIBUTE_TYPE> {
        forward_out!(self.GetItemType(key) -> MF_ATTRIBUTE_TYPE)
    }
    fn CompareItem(
        &self,
        key: *const GUID,
        value: *const PROPVARIANT,
    ) -> windows_core::Result<BOOL> {
        forward_out!(self.CompareItem(key, value.cast()) -> BOOL)
    }
    fn Compare(
        &self,
        theirs: Ref<IMFAttributes>,
        match_type: MF_ATTRIBUTES_MATCH_TYPE,
    ) -> windows_core::Result<BOOL> {
        let theirs = theirs
            .as_ref()
            .map_or(std::ptr::null_mut(), Interface::as_raw);
        forward_out!(self.Compare(theirs, match_type) -> BOOL)
    }
    fn GetUINT32(&self, key: *const GUID) -> windows_core::Result<u32> {
        forward_out!(self.GetUINT32(key) -> u32)
    }
    fn GetUINT64(&self, key: *const GUID) -> windows_core::Result<u64> {
        forward_out!(self.GetUINT64(key) -> u64)
    }
    fn GetDouble(&self, key: *const GUID) -> windows_core::Result<f64> {
        forward_out!(self.GetDouble(key) -> f64)
    }
    fn GetGUID(&self, key: *const GUID) -> windows_core::Result<GUID> {
        forward_out!(self.GetGUID(key) -> GUID)
    }
    fn GetStringLength(&self, key: *const GUID) -> windows_core::Result<u32> {
        forward_out!(self.GetStringLength(key) -> u32)
    }
    fn GetString(
        &self,
        key: *const GUID,
        value: PWSTR,
        size: u32,
        length: *mut u32,
    ) -> windows_core::Result<()> {
        forward!(self.GetString(key, value, size, length))
    }
    fn GetAllocatedString(
        &self,
        key: *const GUID,
        value: *mut PWSTR,
        length: *mut u32,
    ) -> windows_core::Result<()> {
        forward!(self.GetAllocatedString(key, value, length))
    }
    fn GetBlobSize(&self, key: *const GUID) -> windows_core::Result<u32> {
        forward_out!(self.GetBlobSize(key) -> u32)
    }
    fn GetBlob(
        &self,
        key: *const GUID,
        buf: *mut u8,
        size: u32,
        written: *mut u32,
    ) -> windows_core::Result<()> {
        forward!(self.GetBlob(key, buf, size, written))
    }
    fn GetAllocatedBlob(
        &self,
        key: *const GUID,
        buf: *mut *mut u8,
        size: *mut u32,
    ) -> windows_core::Result<()> {
        forward!(self.GetAllocatedBlob(key, buf, size))
    }
    fn GetUnknown(
        &self,
        key: *const GUID,
        iid: *const GUID,
        out: *mut *mut c_void,
    ) -> windows_core::Result<()> {
        forward!(self.GetUnknown(key, iid, out))
    }
    fn SetItem(&self, key: *const GUID, value: *const PROPVARIANT) -> windows_core::Result<()> {
        forward!(self.SetItem(key, value.cast()))
    }
    fn DeleteItem(&self, key: *const GUID) -> windows_core::Result<()> {
        forward!(self.DeleteItem(key))
    }
    fn DeleteAllItems(&self) -> windows_core::Result<()> {
        forward!(self.DeleteAllItems())
    }
    fn SetUINT32(&self, key: *const GUID, value: u32) -> windows_core::Result<()> {
        forward!(self.SetUINT32(key, value))
    }
    fn SetUINT64(&self, key: *const GUID, value: u64) -> windows_core::Result<()> {
        forward!(self.SetUINT64(key, value))
    }
    fn SetDouble(&self, key: *const GUID, value: f64) -> windows_core::Result<()> {
        forward!(self.SetDouble(key, value))
    }
    fn SetGUID(&self, key: *const GUID, value: *const GUID) -> windows_core::Result<()> {
        forward!(self.SetGUID(key, value))
    }
    fn SetString(&self, key: *const GUID, value: &PCWSTR) -> windows_core::Result<()> {
        forward!(self.SetString(key, *value))
    }
    fn SetBlob(&self, key: *const GUID, buf: *const u8, size: u32) -> windows_core::Result<()> {
        forward!(self.SetBlob(key, buf, size))
    }
    fn SetUnknown(&self, key: *const GUID, value: Ref<IUnknown>) -> windows_core::Result<()> {
        let value = value
            .as_ref()
            .map_or(std::ptr::null_mut(), Interface::as_raw);
        forward!(self.SetUnknown(key, value))
    }
    fn LockStore(&self) -> windows_core::Result<()> {
        forward!(self.LockStore())
    }
    fn UnlockStore(&self) -> windows_core::Result<()> {
        forward!(self.UnlockStore())
    }
    fn GetCount(&self) -> windows_core::Result<u32> {
        forward_out!(self.GetCount() -> u32)
    }
    fn GetItemByIndex(
        &self,
        index: u32,
        key: *mut GUID,
        value: *mut PROPVARIANT,
    ) -> windows_core::Result<()> {
        forward!(self.GetItemByIndex(index, key, value.cast()))
    }
    fn CopyAllItems(&self, dest: Ref<IMFAttributes>) -> windows_core::Result<()> {
        let dest = dest
            .as_ref()
            .map_or(std::ptr::null_mut(), Interface::as_raw);
        forward!(self.CopyAllItems(dest))
    }
}
