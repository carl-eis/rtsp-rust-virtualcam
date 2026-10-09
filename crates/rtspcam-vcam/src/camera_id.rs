//! Finding out which app stream a virtual camera belongs to.
//!
//! The app sets the stream id (and its preferred output format) as string attributes on the
//! `IMFVirtualCamera`; Frame Server hands them to the source with its activation object (see
//! `source::resolve_camera`). The lookup below, through the device interface properties, is a
//! fallback for `IMFVirtualCamera::AddProperty`, which Windows refuses for current-user cameras.

use rtspcam_core::constants::{
    CAMERA_FORMAT_PROPERTY_PID, CAMERA_ID_PROPERTY_FMTID, CAMERA_ID_PROPERTY_PID,
};
use uuid::Uuid;
use windows::Win32::Devices::DeviceAndDriverInstallation::{
    CM_Get_DevNode_PropertyW, CM_Get_Device_Interface_PropertyW, CM_LOCATE_DEVNODE_NORMAL,
    CM_Locate_DevNodeW, CR_SUCCESS,
};
use windows::Win32::Devices::Properties::{
    DEVPKEY_Device_InstanceId, DEVPROP_TYPE_STRING, DEVPROPTYPE,
};
use windows::Win32::Foundation::DEVPROPKEY;
use windows_core::{GUID, HSTRING, PCWSTR};

use crate::log::log;

/// What the app told us about this camera.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CameraInfo {
    pub id: Uuid,
    /// The stream's configured output (width, height, fps), offered first.
    pub preferred: Option<(u32, u32, u32)>,
}

fn key(pid: u32) -> DEVPROPKEY {
    DEVPROPKEY {
        fmtid: GUID::from_u128(CAMERA_ID_PROPERTY_FMTID.as_u128()),
        pid,
    }
}

/// Reads the camera properties for the device with this symbolic link.
pub(crate) fn lookup(symbolic_link: &str) -> Option<CameraInfo> {
    let link = HSTRING::from(symbolic_link);
    let read = |pid| {
        interface_string(&link, &key(pid)).or_else(|| {
            let instance = interface_string(&link, &DEVPKEY_Device_InstanceId)?;
            devnode_string(&instance, &key(pid))
        })
    };
    let id = read(CAMERA_ID_PROPERTY_PID)?;
    let id = match Uuid::parse_str(id.trim()) {
        Ok(id) => id,
        Err(_) => {
            log!("camera id property is not a UUID: {id:?}");
            return None;
        }
    };
    let preferred = read(CAMERA_FORMAT_PROPERTY_PID).and_then(|s| parse_format(&s));
    Some(CameraInfo { id, preferred })
}

/// `"1280x720@30"` → (1280, 720, 30).
pub(crate) fn parse_format(s: &str) -> Option<(u32, u32, u32)> {
    let (size, fps) = s.trim().split_once('@')?;
    let (w, h) = size.split_once('x')?;
    Some((w.parse().ok()?, h.parse().ok()?, fps.parse().ok()?))
}

fn interface_string(link: &HSTRING, key: &DEVPROPKEY) -> Option<String> {
    read_string(|ty, buf, len| {
        // SAFETY: `link` is a valid null-terminated string; buffer pointer/length pair is
        // consistent (null with length 0 to query the size).
        unsafe { CM_Get_Device_Interface_PropertyW(PCWSTR(link.as_ptr()), key, ty, buf, len, 0) }
    })
}

fn devnode_string(instance_id: &str, key: &DEVPROPKEY) -> Option<String> {
    let id = HSTRING::from(instance_id);
    let mut devinst = 0u32;
    // SAFETY: valid instance id string and out-param.
    let cr =
        unsafe { CM_Locate_DevNodeW(&mut devinst, PCWSTR(id.as_ptr()), CM_LOCATE_DEVNODE_NORMAL) };
    if cr != CR_SUCCESS {
        return None;
    }
    read_string(|ty, buf, len| {
        // SAFETY: as in `interface_string`.
        unsafe { CM_Get_DevNode_PropertyW(devinst, key, ty, buf, len, 0) }
    })
}

/// Runs a CM property getter twice: once for the size, once for the data.
fn read_string(
    get: impl Fn(
        *mut DEVPROPTYPE,
        Option<*mut u8>,
        *mut u32,
    ) -> windows::Win32::Devices::DeviceAndDriverInstallation::CONFIGRET,
) -> Option<String> {
    let mut ty = DEVPROPTYPE::default();
    let mut len = 0u32;
    let _ = get(&mut ty, None, &mut len);
    if len == 0 || len > 4096 {
        return None;
    }
    let mut buf = vec![0u16; (len as usize).div_ceil(2)];
    if get(&mut ty, Some(buf.as_mut_ptr().cast()), &mut len) != CR_SUCCESS
        || ty != DEVPROP_TYPE_STRING
    {
        return None;
    }
    let s = String::from_utf16_lossy(&buf);
    Some(s.trim_end_matches('\0').to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_formats() {
        assert_eq!(parse_format("1280x720@30"), Some((1280, 720, 30)));
        assert_eq!(parse_format(" 640x480@15 "), Some((640, 480, 15)));
        assert_eq!(parse_format("720p"), None);
    }

    #[test]
    fn unknown_device_is_none() {
        assert_eq!(
            lookup(r"\\?\no-such-device#{00000000-0000-0000-0000-000000000000}"),
            None
        );
    }
}
