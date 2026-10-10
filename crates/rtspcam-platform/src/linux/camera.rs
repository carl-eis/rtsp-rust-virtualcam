//! Virtual cameras through v4l2loopback: each camera is a `/dev/videoN` that the app writes
//! frames to and other apps read like a webcam.
//!
//! - **v4l2loopback 0.13 and later** have a control device, `/dev/v4l2loopback`. Each camera
//!   gets a device of its own, labelled with the stream's name, added in `create` and removed
//!   in `remove`. The package's udev rule gives the logged-in user access to it.
//! - **Older versions** (0.12, in Ubuntu 24.04 and Debian 12) can only make devices when the
//!   module loads. The package loads it with [`FIXED_DEVICES`] devices labelled
//!   "RTSP Cam 1", "RTSP Cam 2", ...; each camera takes a free one, so names don't follow the
//!   streams there.
//!
//! In both cases a device whose label is the stream's name and that no other program writes
//! to is used again, so a device left behind (an app still had it open when its camera was
//! removed, or a crash) isn't duplicated.
//!
//! Frames are YUYV, the format every webcam offers and every Linux camera app reads. With
//! 0.13 and later the device reports when an app starts and stops streaming, so on-demand
//! streams run only while one does; with 0.12 they run while the camera exists.

use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use rtspcam_ipc::{FrameSource, PixelFormat, VideoFormat, placeholder};
use tokio::runtime::Handle;
use uuid::Uuid;

use super::v4l2::{self, CONTROL_DEVICE, Control, Device, LOOPBACK_DRIVER};
use crate::camera::{CameraError, CameraSpec, VirtualCameraBackend};
use crate::push::{FrameSink, Pusher};

/// The label prefix of the devices made at boot for v4l2loopback versions without a control
/// device. The packaging (`installer/linux/rtspcam-v4l2loopback`) uses the same.
const FIXED_LABEL_PREFIX: &str = "RTSP Cam ";

/// How many devices the packaging makes at boot without a control device.
pub(crate) const FIXED_DEVICES: usize = 4;

/// How long to wait for udev to make (and give access to) a device the control device added.
const DEVICE_NODE_WAIT: Duration = Duration::from_secs(3);

/// Cameras on v4l2loopback devices.
#[derive(Debug, Default)]
pub(crate) struct V4l2Loopback {
    state: Mutex<State>,
}

#[derive(Debug, Default)]
struct State {
    cameras: HashMap<Uuid, Camera>,
    /// Devices this process added that were still open when their camera went away. Removed
    /// once nothing has them open.
    lingering: Vec<u32>,
}

#[derive(Debug)]
struct Camera {
    pusher: Pusher,
    /// `N` in `/dev/videoN`.
    nr: u32,
    /// The device can be removed through the control device when the camera goes.
    removable: bool,
}

/// What the module offers, for [`V4l2Loopback::check`] and the error texts.
#[derive(Debug, PartialEq, Eq)]
enum Module {
    Loaded { version: String },
    InstalledNotLoaded,
    Missing,
}

fn module() -> Module {
    if let Ok(version) = fs::read_to_string("/sys/module/v4l2loopback/version") {
        return Module::Loaded {
            version: version.trim().to_owned(),
        };
    }
    if Path::new("/sys/module/v4l2loopback").exists() {
        return Module::Loaded {
            version: "unknown".to_owned(),
        };
    }
    // DKMS runs depmod, so an installed module is listed in modules.dep.
    let installed = fs::read_to_string("/proc/sys/kernel/osrelease")
        .ok()
        .and_then(|release| {
            fs::read_to_string(format!("/lib/modules/{}/modules.dep", release.trim())).ok()
        })
        .is_some_and(|deps| deps.contains("/v4l2loopback.ko"));
    if installed {
        Module::InstalledNotLoaded
    } else {
        Module::Missing
    }
}

impl V4l2Loopback {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl VirtualCameraBackend for V4l2Loopback {
    fn check(&self) -> Result<(), CameraError> {
        match module() {
            Module::Loaded { version } => {
                tracing::debug!(%version, "v4l2loopback is loaded");
                Ok(())
            }
            Module::InstalledNotLoaded => Err(CameraError::Unsupported(
                "The v4l2loopback kernel module is installed but not loaded. Restart the \
                 computer (with Secure Boot, enrol the module's key when asked); see the README"
                    .to_owned(),
            )),
            Module::Missing => Err(CameraError::Unsupported(
                "Virtual cameras need the v4l2loopback kernel module: install \
                 v4l2loopback-dkms (Debian, Ubuntu) or akmod-v4l2loopback (Fedora, from RPM \
                 Fusion), then restart RTSP Cam; see the README"
                    .to_owned(),
            )),
        }
    }

    fn create(
        &self,
        spec: &CameraSpec,
        frames: Arc<dyn FrameSource>,
        _runtime: &Handle,
    ) -> Result<(), CameraError> {
        self.check()?;
        let (width, height, fps) = spec.preferred;
        let format = VideoFormat {
            width,
            height,
            fps,
            pixel_format: PixelFormat::Yuyv,
        };
        let mut state = self.lock();
        // Re-creating replaces the old one (its device may be used again below).
        if let Some(old) = state.cameras.remove(&spec.id) {
            retire(&mut state, old);
        }
        retry_lingering(&mut state);

        let taken: Vec<u32> = state.cameras.values().map(|c| c.nr).collect();
        let (mut device, nr, removable) = claim(&spec.name, &format, &taken)?;
        if let Some(i) = state.lingering.iter().position(|&n| n == nr) {
            state.lingering.remove(i);
        }

        // One picture straight away: with exclusive caps, apps only see a capture device once
        // something has been written to it.
        let (status, message) = frames.status();
        let (title, detail) = placeholder::text_for_status(status, &message);
        if let Err(e) = device.write_frame(&placeholder::render(&format, title, detail)) {
            tracing::warn!(
                camera = spec.name,
                nr,
                "could not write the first frame: {e}"
            );
        }
        let counts_readers = device.watch_clients();
        tracing::info!(
            camera = spec.name,
            device = %device_path(nr).display(),
            %format,
            on_demand_supported = counts_readers,
            "virtual camera ready"
        );

        let sink = DeviceSink {
            device,
            nr,
            failing: false,
        };
        let pusher = Pusher::start(&spec.name, format, frames, Box::new(sink)).map_err(|e| {
            CameraError::Failed(format!("could not start the camera's frame thread: {e}"))
        });
        let pusher = match pusher {
            Ok(p) => p,
            Err(e) => {
                if removable {
                    remove_device(&mut state, nr);
                }
                return Err(e);
            }
        };
        state.cameras.insert(
            spec.id,
            Camera {
                pusher,
                nr,
                removable,
            },
        );
        Ok(())
    }

    fn remove(&self, id: Uuid) {
        let mut state = self.lock();
        if let Some(camera) = state.cameras.remove(&id) {
            retire(&mut state, camera);
        }
        retry_lingering(&mut state);
    }

    fn remove_all(&self) {
        let mut state = self.lock();
        let cameras: Vec<Camera> = state.cameras.drain().map(|(_, c)| c).collect();
        for camera in cameras {
            retire(&mut state, camera);
        }
        retry_lingering(&mut state);
        for nr in &state.lingering {
            tracing::info!(
                nr,
                "an app still has /dev/video{nr} open; it stays until that app closes it and \
                 RTSP Cam starts again, or the next reboot"
            );
        }
    }
}

impl Drop for V4l2Loopback {
    fn drop(&mut self) {
        self.remove_all();
    }
}

fn device_path(nr: u32) -> PathBuf {
    PathBuf::from(format!("/dev/video{nr}"))
}

/// Stops a camera's frames (closing its device) and removes the device if this process may.
fn retire(state: &mut State, camera: Camera) {
    camera.pusher.stop();
    if camera.removable {
        remove_device(state, camera.nr);
    }
}

fn remove_device(state: &mut State, nr: u32) {
    let result = Control::open().and_then(|c| c.remove(nr));
    match result {
        Ok(()) => tracing::debug!(nr, "removed the v4l2loopback device"),
        Err(e) if e.raw_os_error() == Some(libc::EBUSY) => {
            tracing::debug!(nr, "the device is still open; removing it later");
            state.lingering.push(nr);
        }
        Err(e) => tracing::warn!(nr, "could not remove /dev/video{nr}: {e}"),
    }
}

fn retry_lingering(state: &mut State) {
    if state.lingering.is_empty() {
        return;
    }
    let Ok(control) = Control::open() else {
        return;
    };
    state.lingering.retain(|&nr| match control.remove(nr) {
        Ok(()) => false,
        Err(e) => e.raw_os_error() == Some(libc::EBUSY),
    });
}

/// The v4l2loopback devices there are now: number and label.
fn loopback_devices() -> Vec<(u32, String)> {
    let Ok(entries) = fs::read_dir("/sys/class/video4linux") else {
        return Vec::new();
    };
    let mut devices: Vec<(u32, String)> = entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let nr = entry
                .file_name()
                .to_str()?
                .strip_prefix("video")?
                .parse()
                .ok()?;
            let name = fs::read_to_string(entry.path().join("name")).ok()?;
            // Only v4l2loopback devices are virtual (real cameras have a parent device).
            let virtual_device =
                fs::canonicalize(entry.path()).is_ok_and(|p| p.starts_with("/sys/devices/virtual"));
            virtual_device.then(|| (nr, name.trim_end_matches('\n').to_owned()))
        })
        .collect();
    devices.sort();
    devices
}

/// Opens device `nr` and takes it as a writer in `format`. `Ok(None)`: another program
/// writes to it (or it isn't a v4l2loopback device).
fn try_claim(nr: u32, format: &VideoFormat) -> io::Result<Option<Device>> {
    let device = Device::open(&device_path(nr))?;
    let (driver, _) = device.query()?;
    if driver != LOOPBACK_DRIVER {
        return Ok(None);
    }
    match device.set_output_format(format) {
        Ok(()) => {}
        Err(e) if e.raw_os_error() == Some(libc::EBUSY) => return Ok(None),
        Err(e) => return Err(e),
    }
    if let Err(e) = device.set_frame_rate(format.fps) {
        tracing::debug!(nr, "the device did not take the frame rate: {e}");
    }
    Ok(Some(device))
}

/// Finds or makes the device for camera `name`: one already labelled `name`, else a new one
/// through the control device, else a free boot-time device. Skips the numbers in `taken`
/// (this process's other cameras). Returns the device, its number and whether to remove it
/// when the camera goes.
fn claim(
    name: &str,
    format: &VideoFormat,
    taken: &[u32],
) -> Result<(Device, u32, bool), CameraError> {
    let control = Control::open();
    let devices = loopback_devices();
    let label = v4l2::card_label(name);

    for &(nr, _) in devices
        .iter()
        .filter(|(nr, l)| *l == label && !taken.contains(nr))
    {
        match try_claim(nr, format) {
            Ok(Some(device)) => {
                tracing::debug!(nr, "using the existing device labelled {label:?}");
                return Ok((device, nr, control.is_ok()));
            }
            Ok(None) => {}
            Err(e) => tracing::debug!(nr, "could not use /dev/video{nr}: {e}"),
        }
    }

    match &control {
        Ok(control) => {
            let nr = control
                .add(name, format.width, format.height)
                .map_err(|e| add_failed(&e))?;
            match wait_and_claim(nr, format) {
                Ok(device) => return Ok((device, nr, true)),
                Err(e) => {
                    let _ = control.remove(nr);
                    return Err(open_failed(nr, &e));
                }
            }
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => tracing::info!(
            "cannot use {CONTROL_DEVICE} ({e}); looking for devices made at boot instead"
        ),
    }

    let mut last_error = None;
    for &(nr, _) in devices
        .iter()
        .filter(|(nr, l)| l.starts_with(FIXED_LABEL_PREFIX) && !taken.contains(nr))
    {
        match try_claim(nr, format) {
            Ok(Some(device)) => return Ok((device, nr, false)),
            Ok(None) => {}
            Err(e) => last_error = Some((nr, e)),
        }
    }
    if let Some((nr, e)) = last_error {
        return Err(open_failed(nr, &e));
    }
    Err(CameraError::Failed(match control {
        Err(e) if e.kind() == io::ErrorKind::PermissionDenied => format!(
            "No permission to add a camera through {CONTROL_DEVICE}. Install RTSP Cam's \
             package (it adds a udev rule for that), or see the README"
        ),
        _ => format!(
            "No free virtual camera device. This v4l2loopback version makes devices only \
             when it loads; RTSP Cam's package sets up {FIXED_DEVICES} of them (see the README)"
        ),
    }))
}

/// udev makes the node (and the logged-in user's access to it) shortly after the add.
fn wait_and_claim(nr: u32, format: &VideoFormat) -> io::Result<Device> {
    let deadline = Instant::now() + DEVICE_NODE_WAIT;
    loop {
        let result = try_claim(nr, format).and_then(|device| {
            device.ok_or_else(|| io::Error::other("another program took the new device"))
        });
        match result {
            Ok(device) => return Ok(device),
            Err(e)
                if Instant::now() < deadline
                    && matches!(
                        e.kind(),
                        io::ErrorKind::NotFound | io::ErrorKind::PermissionDenied
                    ) =>
            {
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(e) => return Err(e),
        }
    }
}

fn add_failed(e: &io::Error) -> CameraError {
    let why = match e.raw_os_error() {
        Some(libc::ENOSPC | libc::ENFILE) => {
            "v4l2loopback has no room for another device".to_owned()
        }
        _ => e.to_string(),
    };
    CameraError::Failed(format!("could not add a v4l2loopback device: {why}"))
}

fn open_failed(nr: u32, e: &io::Error) -> CameraError {
    let path = device_path(nr);
    if e.kind() == io::ErrorKind::PermissionDenied {
        CameraError::Failed(format!(
            "No permission to open {}. Log out and in again, or add yourself to the `video` \
             group; see the README",
            path.display()
        ))
    } else {
        CameraError::Failed(format!("could not open {}: {e}", path.display()))
    }
}

/// Writes a camera's frames to its device.
#[derive(Debug)]
struct DeviceSink {
    device: Device,
    nr: u32,
    /// Reading the client count failed (logged once; readers aren't counted after that).
    failing: bool,
}

impl FrameSink for DeviceSink {
    fn write(&mut self, frame: &[u8]) -> io::Result<()> {
        self.device.write_frame(frame)
    }

    fn has_reader(&mut self) -> Option<bool> {
        if self.failing {
            return None;
        }
        match self.device.clients() {
            Ok(count) => count.map(|n| n > 0),
            Err(e) => {
                self.failing = true;
                tracing::warn!(
                    nr = self.nr,
                    "could not read who uses /dev/video{}; treating it as always in use: {e}",
                    self.nr
                );
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_labels_match_the_packaging() {
        let script = include_str!("../../../../installer/linux/rtspcam-v4l2loopback");
        assert!(script.contains(&format!("DEVICES={FIXED_DEVICES}")));
        assert!(script.contains(&format!("LABEL_PREFIX=\"{FIXED_LABEL_PREFIX}\"")));
    }
}
