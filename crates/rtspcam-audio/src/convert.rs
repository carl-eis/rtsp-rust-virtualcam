use rubato::{FftFixedIn, Resampler as _};

use crate::block::{BLOCK_LEN, Block, SAMPLE_RATE};
use crate::error::AudioError;

/// Turns sound in a device's format (any rate, any channel count, interleaved `f32`) into
/// 48 kHz stereo [blocks](Block).
///
/// Mono goes to both channels; with more than two channels the first two are kept. A device
/// that isn't at 48 kHz is resampled.
pub struct Converter {
    channels: usize,
    resampler: Option<StereoResampler>,
    /// Interleaved 48 kHz stereo not yet making up a whole block.
    pending: Vec<f32>,
}

impl std::fmt::Debug for Converter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Converter")
            .field("channels", &self.channels)
            .field("resampling", &self.resampler.is_some())
            .finish_non_exhaustive()
    }
}

impl Converter {
    pub fn new(sample_rate: u32, channels: usize) -> Result<Self, AudioError> {
        if channels == 0 || sample_rate == 0 {
            return Err(AudioError::Unsupported(format!(
                "{channels} channels at {sample_rate} Hz"
            )));
        }
        let resampler =
            (sample_rate != SAMPLE_RATE).then(|| StereoResampler::new(sample_rate, SAMPLE_RATE));
        Ok(Self {
            channels,
            resampler: resampler.transpose()?,
            pending: Vec::with_capacity(BLOCK_LEN * 2),
        })
    }

    /// Converts `samples` (interleaved, in the device's format) and hands each block that is
    /// complete to `emit`. Whatever is left over waits for the next call.
    pub fn push(&mut self, samples: &[f32], mut emit: impl FnMut(Block)) {
        let channels = self.channels;
        let frames = samples.chunks_exact(channels).map(|frame| match frame {
            [mono] => (*mono, *mono),
            [left, right, ..] => (*left, *right),
            [] => (0.0, 0.0),
        });
        match &mut self.resampler {
            Some(resampler) => resampler.push(frames, &mut self.pending),
            None => {
                for (l, r) in frames {
                    self.pending.extend([l, r]);
                }
            }
        }
        let whole = self.pending.len() / BLOCK_LEN * BLOCK_LEN;
        for chunk in self.pending[..whole].as_chunks::<BLOCK_LEN>().0 {
            if let Some(block) = Block::from_slice(chunk) {
                emit(block);
            }
        }
        self.pending.drain(..whole);
    }
}

/// Stereo sample-rate conversion in fixed input chunks of 10 ms.
pub(crate) struct StereoResampler {
    inner: FftFixedIn<f32>,
    input: [Vec<f32>; 2],
    output: Vec<Vec<f32>>,
}

impl StereoResampler {
    pub(crate) fn new(from: u32, to: u32) -> Result<Self, AudioError> {
        let chunk = (from as usize / 100).max(1);
        let inner = FftFixedIn::<f32>::new(from as usize, to as usize, chunk, 1, 2)
            .map_err(|e| AudioError::Unsupported(format!("resampling {from} Hz: {e}")))?;
        let output = inner.output_buffer_allocate(true);
        let wanted = inner.input_frames_next();
        Ok(Self {
            inner,
            input: [Vec::with_capacity(wanted), Vec::with_capacity(wanted)],
            output,
        })
    }

    /// Resamples `frames` and appends the result, interleaved, to `out`.
    pub(crate) fn push(&mut self, frames: impl Iterator<Item = (f32, f32)>, out: &mut Vec<f32>) {
        for (l, r) in frames {
            self.input[0].push(l);
            self.input[1].push(r);
            if self.input[0].len() >= self.inner.input_frames_next() {
                self.run(out);
            }
        }
    }

    fn run(&mut self, out: &mut Vec<f32>) {
        match self
            .inner
            .process_into_buffer(&self.input, &mut self.output, None)
        {
            Ok((used, written)) => {
                for i in 0..written {
                    out.extend([self.output[0][i], self.output[1][i]]);
                }
                for channel in &mut self.input {
                    channel.drain(..used.min(channel.len()));
                }
            }
            Err(e) => {
                tracing::warn!(error = %e, "resampling failed; dropping the chunk");
                for channel in &mut self.input {
                    channel.clear();
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::block::BLOCK_FRAMES;

    fn collect(converter: &mut Converter, samples: &[f32]) -> Vec<Block> {
        let mut out = Vec::new();
        converter.push(samples, |b| out.push(b));
        out
    }

    #[test]
    fn stereo_at_48k_passes_through_in_blocks() {
        let mut c = Converter::new(48_000, 2).unwrap();
        let samples: Vec<f32> = (0..BLOCK_LEN + 10).map(|i| i as f32 / 1e5).collect();
        let blocks = collect(&mut c, &samples);
        assert_eq!(blocks.len(), 1);
        assert_eq!(&blocks[0][..], &samples[..BLOCK_LEN]);
        // The rest waits for more.
        let more = vec![0.5; BLOCK_LEN - 10];
        let blocks = collect(&mut c, &more);
        assert_eq!(blocks.len(), 1);
        assert_eq!(&blocks[0][..10], &samples[BLOCK_LEN..]);
    }

    #[test]
    fn mono_goes_to_both_channels() {
        let mut c = Converter::new(48_000, 1).unwrap();
        let samples: Vec<f32> = (0..BLOCK_FRAMES).map(|i| i as f32 / 1e4).collect();
        let blocks = collect(&mut c, &samples);
        assert_eq!(blocks.len(), 1);
        for (i, frame) in blocks[0].as_chunks::<2>().0.iter().enumerate() {
            assert_eq!(*frame, [samples[i], samples[i]]);
        }
    }

    #[test]
    fn more_than_two_channels_keeps_the_first_two() {
        let mut c = Converter::new(48_000, 6).unwrap();
        let samples: Vec<f32> = (0..BLOCK_FRAMES)
            .flat_map(|_| [0.1, 0.2, 0.3, 0.4, 0.5, 0.6])
            .collect();
        let blocks = collect(&mut c, &samples);
        assert_eq!(blocks.len(), 1);
        assert!(
            blocks[0]
                .as_chunks::<2>()
                .0
                .iter()
                .all(|f| *f == [0.1, 0.2])
        );
    }

    #[test]
    fn resampling_keeps_the_duration() {
        for rate in [44_100, 16_000, 96_000] {
            let mut c = Converter::new(rate, 2).unwrap();
            // One second, in device-sized pieces.
            let piece = vec![0.25_f32; rate as usize / 100 * 2 + 6];
            let mut blocks = 0;
            let mut fed = 0;
            while fed < rate as usize {
                c.push(&piece, |_| blocks += 1);
                fed += piece.len() / 2;
            }
            // About 50 blocks per second, give or take the resampler's delay and the extra
            // frames fed by the last piece.
            assert!(
                (47..=52).contains(&blocks),
                "{rate} Hz gave {blocks} blocks"
            );
        }
    }

    #[test]
    fn resampling_keeps_a_steady_level() {
        let mut c = Converter::new(44_100, 1).unwrap();
        let samples = vec![0.5_f32; 44_100];
        let blocks = collect(&mut c, &samples);
        // Past the resampler's start-up, a constant input stays constant.
        let last = blocks.last().unwrap();
        assert!(
            last.iter().all(|s| (s - 0.5).abs() < 0.01),
            "{:?}",
            &last[..8]
        );
    }

    #[test]
    fn rejects_nonsense_formats() {
        assert!(Converter::new(48_000, 0).is_err());
        assert!(Converter::new(0, 2).is_err());
    }
}
