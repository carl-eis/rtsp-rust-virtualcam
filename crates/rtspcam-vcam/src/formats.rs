//! The media types every camera offers: NV12 and RGB32 at 1080p, 720p and 480p, 30 and 15 fps.

use rtspcam_ipc::{PixelFormat, VideoFormat};
use windows::Win32::Media::MediaFoundation::{
    IMFMediaType, MF_MT_ALL_SAMPLES_INDEPENDENT, MF_MT_AVG_BITRATE, MF_MT_DEFAULT_STRIDE,
    MF_MT_FIXED_SIZE_SAMPLES, MF_MT_FRAME_RATE, MF_MT_FRAME_SIZE, MF_MT_INTERLACE_MODE,
    MF_MT_MAJOR_TYPE, MF_MT_PIXEL_ASPECT_RATIO, MF_MT_SAMPLE_SIZE, MF_MT_SUBTYPE,
    MFCreateMediaType, MFMediaType_Video, MFVideoFormat_NV12, MFVideoFormat_RGB32,
    MFVideoInterlace_Progressive,
};

const SIZES: [(u32, u32); 3] = [(1280, 720), (1920, 1080), (640, 480)];
const RATES: [u32; 2] = [30, 15];

/// All formats, best first. `preferred` (the stream's configured output) goes to the front,
/// since many apps simply take the first one.
pub(crate) fn advertised(preferred: Option<(u32, u32, u32)>) -> Vec<VideoFormat> {
    let mut all = Vec::new();
    for pixel_format in [PixelFormat::Nv12, PixelFormat::Rgb32] {
        for fps in RATES {
            for (width, height) in SIZES {
                all.push(VideoFormat {
                    width,
                    height,
                    fps,
                    pixel_format,
                });
            }
        }
    }
    if let Some((w, h, fps)) = preferred
        && let Some(i) = all
            .iter()
            .position(|f| (f.width, f.height, f.fps) == (w, h, fps))
    {
        let f = all.remove(i);
        all.insert(0, f);
    }
    all
}

fn pack(hi: u32, lo: u32) -> u64 {
    (u64::from(hi) << 32) | u64::from(lo)
}

pub(crate) fn media_type(f: &VideoFormat) -> windows_core::Result<IMFMediaType> {
    let (subtype, stride) = match f.pixel_format {
        PixelFormat::Nv12 => (MFVideoFormat_NV12, f.width),
        // Positive stride: top-down rows, which is how frames arrive over IPC.
        PixelFormat::Rgb32 => (MFVideoFormat_RGB32, f.width * 4),
    };
    let size = f.frame_len() as u32;
    // SAFETY: a fresh media type; plain attribute setters.
    unsafe {
        let t = MFCreateMediaType()?;
        t.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
        t.SetGUID(&MF_MT_SUBTYPE, &subtype)?;
        t.SetUINT64(&MF_MT_FRAME_SIZE, pack(f.width, f.height))?;
        t.SetUINT64(&MF_MT_FRAME_RATE, pack(f.fps, 1))?;
        t.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, pack(1, 1))?;
        t.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
        t.SetUINT32(&MF_MT_ALL_SAMPLES_INDEPENDENT, 1)?;
        t.SetUINT32(&MF_MT_FIXED_SIZE_SAMPLES, 1)?;
        t.SetUINT32(&MF_MT_DEFAULT_STRIDE, stride)?;
        t.SetUINT32(&MF_MT_SAMPLE_SIZE, size)?;
        t.SetUINT32(
            &MF_MT_AVG_BITRATE,
            size.saturating_mul(8).saturating_mul(f.fps),
        )?;
        Ok(t)
    }
}

/// Reads back a media type created by [`media_type`] (or one a client built to match).
pub(crate) fn format_of(t: &IMFMediaType) -> Option<VideoFormat> {
    // SAFETY: plain attribute getters on a valid media type.
    unsafe {
        let subtype = t.GetGUID(&MF_MT_SUBTYPE).ok()?;
        let pixel_format = if subtype == MFVideoFormat_NV12 {
            PixelFormat::Nv12
        } else if subtype == MFVideoFormat_RGB32 {
            PixelFormat::Rgb32
        } else {
            return None;
        };
        let size = t.GetUINT64(&MF_MT_FRAME_SIZE).ok()?;
        let rate = t.GetUINT64(&MF_MT_FRAME_RATE).ok()?;
        let (num, den) = ((rate >> 32) as u32, (rate as u32).max(1));
        Some(VideoFormat {
            width: (size >> 32) as u32,
            height: size as u32,
            fps: (num / den).max(1),
            pixel_format,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn twelve_formats_nv12_720p30_first() {
        let all = advertised(None);
        assert_eq!(all.len(), 12);
        assert_eq!(
            all[0],
            VideoFormat {
                width: 1280,
                height: 720,
                fps: 30,
                pixel_format: PixelFormat::Nv12
            }
        );
    }

    #[test]
    fn preferred_goes_first() {
        let all = advertised(Some((1920, 1080, 15)));
        assert_eq!((all[0].width, all[0].height, all[0].fps), (1920, 1080, 15));
        assert_eq!(all[0].pixel_format, PixelFormat::Nv12);
        assert_eq!(all.len(), 12);
        // Unknown preferences are ignored.
        assert_eq!(advertised(Some((1, 1, 1))), advertised(None));
    }
}
