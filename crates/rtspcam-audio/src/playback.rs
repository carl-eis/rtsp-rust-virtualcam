use std::sync::Arc;

use cpal::traits::{DeviceTrait as _, StreamTrait as _};
use cpal::{FromSample, Sample as _, SampleFormat, SizedSample};

use crate::block::SAMPLE_RATE;
use crate::capture::error_handler;
use crate::convert::StereoResampler;
use crate::device::{find_output, name_of};
use crate::error::AudioError;
use crate::queue::BlockQueue;

/// Plays the blocks in a [`BlockQueue`] on an output device until dropped. When the queue runs
/// dry it plays silence.
pub struct Playback {
    _stream: cpal::Stream,
    device_name: String,
}

impl std::fmt::Debug for Playback {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Playback")
            .field("device", &self.device_name)
            .finish_non_exhaustive()
    }
}

impl Playback {
    /// Opens output device `id` (an `output:` id or [`DEFAULT_DEVICE`](crate::DEFAULT_DEVICE))
    /// and starts playing from `queue`.
    pub fn open(
        id: &str,
        queue: Arc<BlockQueue>,
        on_error: impl FnMut(AudioError) + Send + 'static,
    ) -> Result<Self, AudioError> {
        let device = find_output(id)?;
        let supported = device.default_output_config()?;
        let config = supported.config();
        let source = Source::new(queue, config.sample_rate, usize::from(config.channels))?;
        let on_error = error_handler(on_error);
        let stream = match supported.sample_format() {
            SampleFormat::F32 => build::<f32>(&device, &config, source, on_error),
            SampleFormat::I16 => build::<i16>(&device, &config, source, on_error),
            SampleFormat::I32 => build::<i32>(&device, &config, source, on_error),
            SampleFormat::U16 => build::<u16>(&device, &config, source, on_error),
            SampleFormat::U8 => build::<u8>(&device, &config, source, on_error),
            SampleFormat::I8 => build::<i8>(&device, &config, source, on_error),
            SampleFormat::F64 => build::<f64>(&device, &config, source, on_error),
            other => return Err(AudioError::Unsupported(format!("sample format {other}"))),
        }?;
        stream.play()?;
        let device_name = name_of(&device);
        tracing::info!(device = %device_name, rate = config.sample_rate, "audio playback started");
        Ok(Self {
            _stream: stream,
            device_name,
        })
    }

    pub fn device_name(&self) -> &str {
        &self.device_name
    }
}

/// Pulls blocks from the queue and turns them into the device's format.
struct Source {
    queue: Arc<BlockQueue>,
    channels: usize,
    resampler: Option<StereoResampler>,
    /// Interleaved stereo at the device's rate, not played yet.
    ready: Vec<f32>,
    read: usize,
}

impl Source {
    fn new(queue: Arc<BlockQueue>, rate: u32, channels: usize) -> Result<Self, AudioError> {
        if channels == 0 {
            return Err(AudioError::Unsupported("an output with no channels".into()));
        }
        let resampler = (rate != SAMPLE_RATE).then(|| StereoResampler::new(SAMPLE_RATE, rate));
        Ok(Self {
            queue,
            channels,
            resampler: resampler.transpose()?,
            ready: Vec::new(),
            read: 0,
        })
    }

    /// The next stereo frame; silence when nothing is queued.
    fn next_frame(&mut self) -> (f32, f32) {
        if self.read + 1 >= self.ready.len() {
            self.ready.clear();
            self.read = 0;
            match self.queue.pop() {
                Some(block) => match &mut self.resampler {
                    Some(resampler) => resampler.push(
                        block.as_chunks::<2>().0.iter().map(|f| (f[0], f[1])),
                        &mut self.ready,
                    ),
                    None => self.ready.extend_from_slice(&block),
                },
                None => return (0.0, 0.0),
            }
            if self.ready.len() < 2 {
                return (0.0, 0.0);
            }
        }
        let frame = (self.ready[self.read], self.ready[self.read + 1]);
        self.read += 2;
        frame
    }

    fn fill<T: SizedSample + FromSample<f32>>(&mut self, out: &mut [T]) {
        let channels = self.channels;
        for frame in out.chunks_exact_mut(channels) {
            let (l, r) = self.next_frame();
            match frame {
                [mono] => *mono = ((l + r) * 0.5).to_sample(),
                [left, right, rest @ ..] => {
                    *left = l.to_sample();
                    *right = r.to_sample();
                    for s in rest {
                        *s = 0.0_f32.to_sample();
                    }
                }
                [] => {}
            }
        }
    }
}

fn build<T>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    mut source: Source,
    on_error: impl FnMut(cpal::Error) + Send + 'static,
) -> Result<cpal::Stream, AudioError>
where
    T: SizedSample + FromSample<f32>,
{
    let stream = device.build_output_stream::<T, _, _>(
        *config,
        move |out: &mut [T], _| source.fill(out),
        on_error,
        None,
    )?;
    Ok(stream)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::block::{BLOCK_FRAMES, BLOCK_LEN, Block};

    #[test]
    fn plays_queued_blocks_then_silence() {
        let queue = Arc::new(BlockQueue::default());
        let mut block = Block::silence();
        for frame in block.as_chunks_mut::<2>().0 {
            frame.copy_from_slice(&[0.25, -0.25]);
        }
        queue.push(block);
        let mut source = Source::new(queue, SAMPLE_RATE, 2).unwrap();
        let mut out = vec![1.0_f32; BLOCK_LEN + 4];
        source.fill(&mut out);
        assert!(
            out[..BLOCK_LEN]
                .as_chunks::<2>()
                .0
                .iter()
                .all(|f| *f == [0.25, -0.25])
        );
        assert_eq!(&out[BLOCK_LEN..], [0.0; 4]);
    }

    #[test]
    fn mono_and_surround_outputs() {
        let queue = Arc::new(BlockQueue::default());
        let mut block = Block::silence();
        for frame in block.as_chunks_mut::<2>().0 {
            frame.copy_from_slice(&[0.5, 0.25]);
        }
        queue.push(block.clone());
        let mut mono = Source::new(queue.clone(), SAMPLE_RATE, 1).unwrap();
        let mut out = vec![0.0_f32; BLOCK_FRAMES];
        mono.fill(&mut out);
        assert!(out.iter().all(|s| *s == 0.375));

        queue.push(block);
        let mut six = Source::new(queue, SAMPLE_RATE, 6).unwrap();
        let mut out = vec![1.0_f32; 12];
        six.fill(&mut out);
        assert_eq!(&out[..6], [0.5, 0.25, 0.0, 0.0, 0.0, 0.0]);
    }

    #[test]
    fn integer_outputs_are_converted() {
        let queue = Arc::new(BlockQueue::default());
        queue.push(Block::from_slice(&[0.5; BLOCK_LEN]).unwrap());
        let mut source = Source::new(queue, SAMPLE_RATE, 2).unwrap();
        let mut out = vec![0_i16; 4];
        source.fill(&mut out);
        assert!(out.iter().all(|s| (*s - 16384).abs() <= 1), "{out:?}");
    }
}
