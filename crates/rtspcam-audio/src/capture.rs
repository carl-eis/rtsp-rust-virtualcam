use cpal::traits::{DeviceTrait as _, StreamTrait as _};
use cpal::{FromSample, SampleFormat, SizedSample};

use crate::block::Block;
use crate::convert::Converter;
use crate::device::{find_source, name_of};
use crate::error::AudioError;

/// What a [`Capture`] opened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaptureInfo {
    pub device_name: String,
    /// The device's own rate and channel count (converted to 48 kHz stereo).
    pub sample_rate: u32,
    pub channels: u16,
    /// Recording what an output device plays.
    pub loopback: bool,
}

/// Records from a device and hands out 48 kHz stereo blocks until dropped.
///
/// `on_block` runs on the audio system's own thread, once per 20 ms block: it must not block
/// (no waiting on locks someone else may hold for long, no I/O).
pub struct Capture {
    _stream: cpal::Stream,
    info: CaptureInfo,
}

impl std::fmt::Debug for Capture {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Capture")
            .field("info", &self.info)
            .finish_non_exhaustive()
    }
}

impl Capture {
    /// Opens device `id` (see [`DeviceInfo::id`](crate::DeviceInfo::id), or
    /// [`DEFAULT_DEVICE`](crate::DEFAULT_DEVICE)) and starts recording. `on_error` is called
    /// once if the device stops working (for example when it is unplugged); the capture then
    /// delivers nothing more and should be dropped.
    pub fn open(
        id: &str,
        on_block: impl FnMut(Block) + Send + 'static,
        on_error: impl FnMut(AudioError) + Send + 'static,
    ) -> Result<Self, AudioError> {
        let (device, loopback) = find_source(id)?;
        let supported = if loopback {
            device.default_output_config()?
        } else {
            device.default_input_config()?
        };
        let config = supported.config();
        let info = CaptureInfo {
            device_name: name_of(&device),
            sample_rate: config.sample_rate,
            channels: config.channels,
            loopback,
        };
        let converter = Converter::new(config.sample_rate, usize::from(config.channels))?;
        let on_error = error_handler(on_error);
        let stream = match supported.sample_format() {
            SampleFormat::F32 => build::<f32>(&device, &config, converter, on_block, on_error),
            SampleFormat::I16 => build::<i16>(&device, &config, converter, on_block, on_error),
            SampleFormat::I32 => build::<i32>(&device, &config, converter, on_block, on_error),
            SampleFormat::U16 => build::<u16>(&device, &config, converter, on_block, on_error),
            SampleFormat::U8 => build::<u8>(&device, &config, converter, on_block, on_error),
            SampleFormat::I8 => build::<i8>(&device, &config, converter, on_block, on_error),
            SampleFormat::F64 => build::<f64>(&device, &config, converter, on_block, on_error),
            other => return Err(AudioError::Unsupported(format!("sample format {other}"))),
        }?;
        stream.play()?;
        tracing::info!(
            device = %info.device_name,
            rate = info.sample_rate,
            channels = info.channels,
            loopback,
            "audio capture started"
        );
        Ok(Self {
            _stream: stream,
            info,
        })
    }

    pub fn info(&self) -> &CaptureInfo {
        &self.info
    }
}

fn build<T>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    mut converter: Converter,
    mut on_block: impl FnMut(Block) + Send + 'static,
    on_error: impl FnMut(cpal::Error) + Send + 'static,
) -> Result<cpal::Stream, AudioError>
where
    T: SizedSample,
    f32: FromSample<T>,
{
    let mut scratch: Vec<f32> = Vec::new();
    let stream = device.build_input_stream::<T, _, _>(
        *config,
        move |data: &[T], _| {
            scratch.clear();
            scratch.extend(data.iter().map(|s| s.to_sample::<f32>()));
            converter.push(&scratch, &mut on_block);
        },
        on_error,
        None,
    )?;
    Ok(stream)
}

/// Passes on the errors that end the stream, once; overruns and the like only go to the log.
pub(crate) fn error_handler(
    mut on_error: impl FnMut(AudioError) + Send + 'static,
) -> impl FnMut(cpal::Error) + Send + 'static {
    let mut reported = false;
    move |e: cpal::Error| {
        use cpal::ErrorKind;
        match e.kind() {
            ErrorKind::Xrun | ErrorKind::DeviceChanged | ErrorKind::RealtimeDenied => {
                tracing::debug!(error = %e, "audio stream hiccup");
            }
            _ if reported => {}
            _ => {
                tracing::warn!(error = %e, "audio stream failed");
                reported = true;
                on_error(AudioError::from(e));
            }
        }
    }
}
