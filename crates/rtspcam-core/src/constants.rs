//! Names and identifiers shared between the app, the CLI and the virtual camera DLL.

use uuid::Uuid;

/// Folder name used under `%APPDATA%`, `%LOCALAPPDATA%` and `C:\Program Files`.
pub const APP_DIR_NAME: &str = "RtspCam";

/// Human-readable product name.
pub const APP_DISPLAY_NAME: &str = "RTSP Cam";

/// File name of the JSON configuration.
pub const CONFIG_FILE_NAME: &str = "config.json";

/// CLSID of the Media Foundation custom media source in `rtspcam_vcam.dll`.
pub const VCAM_SOURCE_CLSID: Uuid = uuid::uuid!("d533721f-03a1-4d9e-b562-31e82d1ed87f");

/// Prefix of the per-camera named pipes served by the app (see [`frame_pipe_name`]).
pub const PIPE_PREFIX: &str = r"\\.\pipe\rtspcam\";

/// Named pipe the virtual camera for stream `id` connects to for frames and control messages.
pub fn frame_pipe_name(id: Uuid) -> String {
    format!("{PIPE_PREFIX}{}", id.as_hyphenated())
}

/// The CLSID formatted the way registry keys and `MFCreateVirtualCamera` expect it:
/// upper-case, hyphenated, with braces.
pub fn vcam_source_clsid_string() -> String {
    format!("{{{}}}", VCAM_SOURCE_CLSID.as_hyphenated()).to_uppercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clsid_string_has_braces() {
        assert_eq!(vcam_source_clsid_string(), "{D533721F-03A1-4D9E-B562-31E82D1ED87F}");
    }

    #[test]
    fn pipe_name_contains_id() {
        let id = uuid::uuid!("6f1c2d3e-8a4b-4c5d-9e0f-112233445566");
        assert_eq!(
            frame_pipe_name(id),
            r"\\.\pipe\rtspcam\6f1c2d3e-8a4b-4c5d-9e0f-112233445566"
        );
    }
}
