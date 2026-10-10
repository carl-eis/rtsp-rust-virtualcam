//! The few V4L2 and v4l2loopback ioctls the camera backend needs, on top of `libc`.
//!
//! Struct layouts follow `<linux/videodev2.h>` and v4l2loopback's `v4l2loopback.h`; the tests
//! pin their sizes (which are part of the ioctl numbers) on 64-bit targets.

use std::ffi::c_void;
use std::fs::{File, OpenOptions};
use std::io::{self, Write as _};
use std::os::fd::AsRawFd as _;
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::Path;

use rtspcam_ipc::{PixelFormat, VideoFormat};

/// The control device v4l2loopback 0.13 and later make for adding and removing devices.
pub(crate) const CONTROL_DEVICE: &str = "/dev/v4l2loopback";

/// What `VIDIOC_QUERYCAP` reports as the driver of a v4l2loopback device.
pub(crate) const LOOPBACK_DRIVER: &str = "v4l2 loopback";

const V4L2_BUF_TYPE_VIDEO_OUTPUT: u32 = 2;
const V4L2_FIELD_NONE: u32 = 1;
const V4L2_COLORSPACE_SRGB: u32 = 8;
const V4L2_EVENT_PRIVATE_START: u32 = 0x0800_0000;
/// v4l2loopback's event with the number of apps streaming from the device (0 or 1).
const V4L2_EVENT_PRI_CLIENT_USAGE: u32 = V4L2_EVENT_PRIVATE_START + 0x08E0_0000 + 1;

#[repr(C)]
struct Capability {
    driver: [u8; 16],
    card: [u8; 32],
    bus_info: [u8; 32],
    version: u32,
    capabilities: u32,
    device_caps: u32,
    reserved: [u32; 3],
}

#[repr(C)]
#[derive(Clone, Copy)]
struct PixFormat {
    width: u32,
    height: u32,
    pixelformat: u32,
    field: u32,
    bytesperline: u32,
    sizeimage: u32,
    colorspace: u32,
    priv_: u32,
    flags: u32,
    ycbcr_enc: u32,
    quantization: u32,
    xfer_func: u32,
}

/// `struct v4l2_format`. The C union holds pointers, so it is pointer-aligned.
#[repr(C)]
struct Format {
    type_: u32,
    fmt: FormatUnion,
}

#[repr(C)]
union FormatUnion {
    pix: PixFormat,
    raw: [u8; 200],
    _align: [usize; 0],
}

#[repr(C)]
#[derive(Clone, Copy)]
struct Fract {
    numerator: u32,
    denominator: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct OutputParm {
    capability: u32,
    outputmode: u32,
    timeperframe: Fract,
    extendedmode: u32,
    writebuffers: u32,
    reserved: [u32; 4],
}

#[repr(C)]
struct StreamParm {
    type_: u32,
    parm: StreamParmUnion,
}

#[repr(C)]
union StreamParmUnion {
    output: OutputParm,
    raw: [u8; 200],
}

#[repr(C)]
struct EventSubscription {
    type_: u32,
    id: u32,
    flags: u32,
    reserved: [u32; 5],
}

/// `struct v4l2_event`. The C union holds 64-bit values, so it is 8-byte aligned.
#[repr(C)]
struct Event {
    type_: u32,
    u: EventUnion,
    pending: u32,
    sequence: u32,
    timestamp: libc::timespec,
    id: u32,
    reserved: [u32; 8],
}

#[repr(C)]
union EventUnion {
    data: [u8; 64],
    _align: [u64; 0],
}

/// `struct v4l2_loopback_config`, the same in v4l2loopback 0.13 to 0.15.
#[repr(C)]
struct LoopbackConfig {
    output_nr: i32,
    unused: i32,
    card_label: [u8; 32],
    min_width: u32,
    max_width: u32,
    min_height: u32,
    max_height: u32,
    max_buffers: i32,
    max_openers: i32,
    debug: i32,
    announce_all_caps: i32,
}

const VIDIOC_QUERYCAP: libc::Ioctl = libc::_IOR::<Capability>(b'V' as u32, 0);
const VIDIOC_S_FMT: libc::Ioctl = libc::_IOWR::<Format>(b'V' as u32, 5);
const VIDIOC_S_PARM: libc::Ioctl = libc::_IOWR::<StreamParm>(b'V' as u32, 22);
const VIDIOC_DQEVENT: libc::Ioctl = libc::_IOR::<Event>(b'V' as u32, 89);
const VIDIOC_SUBSCRIBE_EVENT: libc::Ioctl = libc::_IOW::<EventSubscription>(b'V' as u32, 90);
// The original numbers, which every version with a control device accepts (0.15 added
// `_IOW`-style ones and kept these).
const V4L2LOOPBACK_CTL_ADD: libc::Ioctl = 0x4C80 as libc::Ioctl;
const V4L2LOOPBACK_CTL_REMOVE: libc::Ioctl = 0x4C81 as libc::Ioctl;

/// Runs one ioctl that takes a pointer (or, for `CTL_REMOVE`, a number) and returns its result.
///
/// # Safety
///
/// `arg` must be what the kernel expects for `request` on `file`: a pointer to a live value of
/// the request's type, or a plain number for requests that take one.
unsafe fn ioctl(file: &File, request: libc::Ioctl, arg: *mut c_void) -> io::Result<i32> {
    loop {
        // SAFETY: the caller guarantees `arg` matches `request`; the fd is open for the call.
        let r = unsafe { libc::ioctl(file.as_raw_fd(), request, arg) };
        if r >= 0 {
            return Ok(r);
        }
        let e = io::Error::last_os_error();
        if e.kind() != io::ErrorKind::Interrupted {
            return Err(e);
        }
    }
}

fn fourcc(code: &[u8; 4]) -> u32 {
    u32::from_le_bytes(*code)
}

fn c_string(bytes: &[u8]) -> String {
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    String::from_utf8_lossy(&bytes[..end]).into_owned()
}

/// Copies `label` into a NUL-terminated C buffer, cut at a character boundary if too long.
fn label_bytes(label: &str) -> [u8; 32] {
    let mut out = [0u8; 32];
    let mut end = label.len().min(out.len() - 1);
    while !label.is_char_boundary(end) {
        end -= 1;
    }
    out[..end].copy_from_slice(&label.as_bytes()[..end]);
    out
}

/// The `card_label` a device gets for `label`: what [`Device::card`] reads back.
pub(crate) fn card_label(label: &str) -> String {
    c_string(&label_bytes(label))
}

/// v4l2loopback's control device.
#[derive(Debug)]
pub(crate) struct Control(File);

impl Control {
    pub(crate) fn open() -> io::Result<Self> {
        OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_CLOEXEC)
            .open(CONTROL_DEVICE)
            .map(Self)
    }

    /// Adds a device labelled `label` that announces only one of output or capture at a time
    /// (what Chrome needs to list it). Returns its number, `N` in `/dev/videoN`.
    pub(crate) fn add(&self, label: &str, max_width: u32, max_height: u32) -> io::Result<u32> {
        let mut config = LoopbackConfig {
            output_nr: -1,
            unused: -1,
            card_label: label_bytes(label),
            min_width: 0,
            max_width,
            min_height: 0,
            max_height,
            max_buffers: 0,
            max_openers: 0,
            debug: 0,
            announce_all_caps: 0,
        };
        // SAFETY: CTL_ADD takes a pointer to a `v4l2_loopback_config`, which `config` is.
        let nr = unsafe {
            ioctl(
                &self.0,
                V4L2LOOPBACK_CTL_ADD,
                (&raw mut config).cast::<c_void>(),
            )?
        };
        Ok(nr as u32)
    }

    /// Removes device `nr`. Fails with `EBUSY` while anything has it open.
    pub(crate) fn remove(&self, nr: u32) -> io::Result<()> {
        // SAFETY: CTL_REMOVE takes the device number itself, not a pointer.
        unsafe { ioctl(&self.0, V4L2LOOPBACK_CTL_REMOVE, nr as usize as *mut c_void)? };
        Ok(())
    }
}

/// An open `/dev/videoN`, written to as a video output.
#[derive(Debug)]
pub(crate) struct Device {
    file: File,
    /// Apps streaming from the device, from v4l2loopback's client-usage events. `None` when
    /// the module doesn't send them (before 0.13).
    clients: Option<u32>,
}

impl Device {
    pub(crate) fn open(path: &Path) -> io::Result<Self> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_CLOEXEC)
            .open(path)?;
        Ok(Self {
            file,
            clients: None,
        })
    }

    /// The driver's name and the card label, from `VIDIOC_QUERYCAP`.
    pub(crate) fn query(&self) -> io::Result<(String, String)> {
        // SAFETY: all-zero bytes are a valid `Capability` (integers and byte arrays).
        let mut cap: Capability = unsafe { std::mem::zeroed() };
        // SAFETY: QUERYCAP fills a `v4l2_capability`, which `cap` is.
        unsafe { ioctl(&self.file, VIDIOC_QUERYCAP, (&raw mut cap).cast())? };
        Ok((c_string(&cap.driver), c_string(&cap.card)))
    }

    /// Sets the format this device's readers get. `EBUSY` when another writer has it.
    pub(crate) fn set_output_format(&self, format: &VideoFormat) -> io::Result<()> {
        let (pixelformat, bytesperline) = match format.pixel_format {
            PixelFormat::Yuyv => (fourcc(b"YUYV"), format.width * 2),
            PixelFormat::I420 => (fourcc(b"YU12"), format.width),
            PixelFormat::Nv12 => (fourcc(b"NV12"), format.width),
            PixelFormat::Rgb32 => (fourcc(b"XR24"), format.width * 4),
        };
        let mut fmt = Format {
            type_: V4L2_BUF_TYPE_VIDEO_OUTPUT,
            fmt: FormatUnion { raw: [0; 200] },
        };
        fmt.fmt.pix = PixFormat {
            width: format.width,
            height: format.height,
            pixelformat,
            field: V4L2_FIELD_NONE,
            bytesperline,
            sizeimage: format.frame_len() as u32,
            colorspace: V4L2_COLORSPACE_SRGB,
            priv_: 0,
            flags: 0,
            ycbcr_enc: 0,
            quantization: 0,
            xfer_func: 0,
        };
        // SAFETY: S_FMT takes a `v4l2_format`, which `fmt` is.
        unsafe { ioctl(&self.file, VIDIOC_S_FMT, (&raw mut fmt).cast())? };

        // The driver can adjust the format; a different one would garble every frame.
        // SAFETY: the kernel filled the `pix` member, the one S_FMT uses for this buffer type.
        let got = unsafe { fmt.fmt.pix };
        if (got.width, got.height, got.pixelformat) != (format.width, format.height, pixelformat) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "the device took {}x{} instead of {}x{}",
                    got.width, got.height, format.width, format.height
                ),
            ));
        }
        Ok(())
    }

    /// Tells readers the frame rate. Older modules ignore it.
    pub(crate) fn set_frame_rate(&self, fps: u32) -> io::Result<()> {
        let mut parm = StreamParm {
            type_: V4L2_BUF_TYPE_VIDEO_OUTPUT,
            parm: StreamParmUnion { raw: [0; 200] },
        };
        parm.parm.output = OutputParm {
            capability: 0,
            outputmode: 0,
            timeperframe: Fract {
                numerator: 1,
                denominator: fps.max(1),
            },
            extendedmode: 0,
            writebuffers: 0,
            reserved: [0; 4],
        };
        // SAFETY: S_PARM takes a `v4l2_streamparm`, which `parm` is.
        unsafe { ioctl(&self.file, VIDIOC_S_PARM, (&raw mut parm).cast())? };
        Ok(())
    }

    /// Asks for v4l2loopback's client-usage events. Returns `false` when the module has none
    /// (before 0.13), so readers can't be counted.
    pub(crate) fn watch_clients(&mut self) -> bool {
        let mut sub = EventSubscription {
            type_: V4L2_EVENT_PRI_CLIENT_USAGE,
            id: 0,
            flags: 0,
            reserved: [0; 5],
        };
        // SAFETY: SUBSCRIBE_EVENT takes a `v4l2_event_subscription`, which `sub` is.
        let ok = unsafe { ioctl(&self.file, VIDIOC_SUBSCRIBE_EVENT, (&raw mut sub).cast()) };
        match ok {
            Ok(_) => {
                // The module queues the current count right away; start from none until then.
                self.clients = Some(0);
                true
            }
            Err(e) => {
                tracing::debug!("no client-usage events from v4l2loopback: {e}");
                false
            }
        }
    }

    /// How many apps stream from the device, after reading any waiting events. `None` when
    /// [`watch_clients`](Self::watch_clients) failed.
    pub(crate) fn clients(&mut self) -> io::Result<Option<u32>> {
        if self.clients.is_none() {
            return Ok(None);
        }
        loop {
            let mut poll = libc::pollfd {
                fd: self.file.as_raw_fd(),
                events: libc::POLLPRI,
                revents: 0,
            };
            // SAFETY: one valid `pollfd`, and a zero timeout.
            let n = unsafe { libc::poll(&raw mut poll, 1, 0) };
            if n < 0 {
                let e = io::Error::last_os_error();
                if e.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(e);
            }
            if n == 0 || poll.revents & libc::POLLPRI == 0 {
                return Ok(self.clients);
            }
            // SAFETY: all-zero bytes are a valid `Event` (integers and byte arrays).
            let mut event: Event = unsafe { std::mem::zeroed() };
            // SAFETY: DQEVENT fills a `v4l2_event`, which `event` is. An event is waiting
            // (POLLPRI), so it doesn't block.
            unsafe { ioctl(&self.file, VIDIOC_DQEVENT, (&raw mut event).cast())? };
            if event.type_ == V4L2_EVENT_PRI_CLIENT_USAGE {
                // SAFETY: the data member is plain bytes; the event's payload is a `u32`.
                let data = unsafe { event.u.data };
                let count = u32::from_ne_bytes([data[0], data[1], data[2], data[3]]);
                self.clients = Some(count);
            }
        }
    }

    /// Writes one frame (exactly the size [`set_output_format`](Self::set_output_format) set).
    pub(crate) fn write_frame(&mut self, frame: &[u8]) -> io::Result<()> {
        self.file.write_all(frame)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg(target_pointer_width = "64")]
    fn struct_sizes_match_the_kernel_headers() {
        // From <linux/videodev2.h> on x86-64 and arm64.
        assert_eq!(size_of::<Capability>(), 104);
        assert_eq!(size_of::<PixFormat>(), 48);
        assert_eq!(size_of::<Format>(), 208);
        assert_eq!(size_of::<StreamParm>(), 204);
        assert_eq!(size_of::<EventSubscription>(), 32);
        assert_eq!(size_of::<Event>(), 136);
        assert_eq!(size_of::<LoopbackConfig>(), 72);
    }

    #[test]
    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    #[allow(clippy::unnecessary_cast, reason = "`Ioctl` is `c_int` on musl")]
    fn ioctl_numbers_match_the_kernel_headers() {
        assert_eq!(VIDIOC_QUERYCAP as u64, 0x8068_5600);
        assert_eq!(VIDIOC_S_FMT as u64, 0xc0d0_5605);
        assert_eq!(VIDIOC_S_PARM as u64, 0xc0cc_5616);
        assert_eq!(VIDIOC_DQEVENT as u64, 0x8088_5659);
        assert_eq!(VIDIOC_SUBSCRIBE_EVENT as u64, 0x4020_565a);
    }

    #[test]
    fn labels_are_cut_to_fit() {
        assert_eq!(card_label("Front door"), "Front door");
        let long = "A camera name that is much longer than thirty-one bytes";
        assert_eq!(card_label(long), &long[..31]);
        // Not in the middle of a character.
        let accents = "ééééééééééééééééé"; // 34 bytes
        assert_eq!(card_label(accents), "ééééééééééééééé");
        assert_eq!(fourcc(b"YUYV"), 0x5659_5559);
    }
}
