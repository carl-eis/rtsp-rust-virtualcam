//! The media types every camera offers: NV12, YUY2, I420 and RGB32 at 1080p, 720p and 480p, 30,
//! 15 and 60 fps, plus the camera's own size when its stream is set to "Source". NV12 comes
//! first: it is what the app decodes to, so it costs no conversion. YUY2 is what most USB
//! webcams offer, and some apps look for it first.

use rtspcam_ipc::protocol::{MAX_HEIGHT, MAX_WIDTH};
use rtspcam_ipc::{PixelFormat, VideoFormat};
use windows::Win32::Media::MediaFoundation::{
    IMFMediaType, MF_MT_ALL_SAMPLES_INDEPENDENT, MF_MT_AVG_BITRATE, MF_MT_DEFAULT_STRIDE,
    MF_MT_FIXED_SIZE_SAMPLES, MF_MT_FRAME_RATE, MF_MT_FRAME_SIZE, MF_MT_INTERLACE_MODE,
    MF_MT_MAJOR_TYPE, MF_MT_PIXEL_ASPECT_RATIO, MF_MT_SAMPLE_SIZE, MF_MT_SUBTYPE,
    MFCreateMediaType, MFMediaType_Video, MFVideoFormat_I420, MFVideoFormat_NV12,
    MFVideoFormat_RGB32, MFVideoFormat_YUY2, MFVideoInterlace_Progressive,
};

const SIZES: [(u32, u32); 3] = [(1280, 720), (1920, 1080), (640, 480)];
const RATES: [u32; 3] = [30, 15, 60];

/// All formats, best first. `preferred` (the stream's configured output) goes to the front,
/// since many apps simply take the first one. A preferred size that isn't one of the usual
/// ones (a "Source" output: the camera's own size) is offered as well, in every pixel format
/// and rate, if it is even and within the IPC limits.
pub(crate) fn advertised(preferred: Option<(u32, u32, u32)>) -> Vec<VideoFormat> {
    let mut sizes = SIZES.to_vec();
    if let Some((w, h, _)) = preferred
        && !sizes.contains(&(w, h))
        && w % 2 == 0
        && h % 2 == 0
        && (2..=MAX_WIDTH).contains(&w)
        && (2..=MAX_HEIGHT).contains(&h)
    {
        sizes.insert(0, (w, h));
    }
    let mut all = Vec::new();
    for pixel_format in [
        PixelFormat::Nv12,
        PixelFormat::Yuyv,
        PixelFormat::I420,
        PixelFormat::Rgb32,
    ] {
        for fps in RATES {
            for &(width, height) in &sizes {
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
        PixelFormat::Yuyv => (MFVideoFormat_YUY2, f.width * 2),
        // The stride of the Y plane; each chroma plane has half of it.
        PixelFormat::I420 => (MFVideoFormat_I420, f.width),
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
        } else if subtype == MFVideoFormat_YUY2 {
            PixelFormat::Yuyv
        } else if subtype == MFVideoFormat_I420 {
            PixelFormat::I420
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
    fn thirty_six_formats_nv12_720p30_first() {
        // 4 pixel formats x 3 rates x 3 sizes.
        let all = advertised(None);
        assert_eq!(all.len(), 36);
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
        assert_eq!(all.len(), 36);
        let all = advertised(Some((640, 480, 60)));
        assert_eq!((all[0].width, all[0].height, all[0].fps), (640, 480, 60));
        // Sizes that can't be sent are ignored.
        assert_eq!(advertised(Some((1, 1, 1))), advertised(None));
        assert_eq!(advertised(Some((1281, 720, 30))), advertised(None));
        assert_eq!(advertised(Some((4098, 2160, 30))), advertised(None));
    }

    #[test]
    fn the_source_size_is_offered_besides_the_usual_ones() {
        let all = advertised(Some((2560, 1440, 60)));
        assert_eq!(all.len(), 48, "4 pixel formats x 3 rates x 4 sizes");
        assert_eq!(
            all[0],
            VideoFormat {
                width: 2560,
                height: 1440,
                fps: 60,
                pixel_format: PixelFormat::Nv12
            }
        );
        for pixel_format in [PixelFormat::Yuyv, PixelFormat::I420, PixelFormat::Rgb32] {
            for fps in RATES {
                assert!(all.contains(&VideoFormat {
                    width: 2560,
                    height: 1440,
                    fps,
                    pixel_format
                }));
            }
        }
        // The usual sizes are all still there.
        for (w, h) in SIZES {
            assert_eq!(
                all.iter().filter(|f| (f.width, f.height) == (w, h)).count(),
                12
            );
        }
        // The largest picture the pipe carries.
        let all = advertised(Some((4096, 2160, 30)));
        assert_eq!(all.len(), 48);
        assert_eq!((all[0].width, all[0].height), (4096, 2160));
    }

    #[test]
    fn every_pixel_format_offered_and_read_back() {
        let all = advertised(None);
        for pixel_format in [
            PixelFormat::Nv12,
            PixelFormat::Yuyv,
            PixelFormat::I420,
            PixelFormat::Rgb32,
        ] {
            assert_eq!(
                all.iter()
                    .filter(|f| f.pixel_format == pixel_format)
                    .count(),
                9
            );
        }
        // All NV12 types come before any other.
        assert!(all[..9].iter().all(|f| f.pixel_format == PixelFormat::Nv12));
        let mut all = all;
        all.extend(advertised(Some((2560, 1440, 60))));
        for f in &all {
            let t = media_type(f).unwrap();
            assert_eq!(format_of(&t), Some(*f));
            // SAFETY: plain attribute getters on a valid media type.
            let (stride, size) = unsafe {
                (
                    t.GetUINT32(&MF_MT_DEFAULT_STRIDE).unwrap(),
                    t.GetUINT32(&MF_MT_SAMPLE_SIZE).unwrap(),
                )
            };
            let expected = match f.pixel_format {
                PixelFormat::Nv12 | PixelFormat::I420 => f.width,
                PixelFormat::Yuyv => f.width * 2,
                PixelFormat::Rgb32 => f.width * 4,
            };
            assert_eq!(stride, expected, "{f}");
            assert_eq!(size as usize, f.frame_len(), "{f}");
        }
    }
}
