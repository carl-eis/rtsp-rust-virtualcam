//! Mixing several voices into one block: sum, then soft-clip.

use crate::block::{BLOCK_LEN, Block};

/// Below this level samples pass unchanged; above it they bend smoothly towards 1.0.
const KNEE: f32 = 0.8;

/// Sums voices into one block. Each voice is 16-bit interleaved stereo at 48 kHz (what a
/// Discord voice decoder gives); `gain` scales them (1.0 = as received).
#[derive(Debug)]
pub struct Mix {
    block: Block,
    voices: usize,
}

impl Default for Mix {
    fn default() -> Self {
        Self::new()
    }
}

impl Mix {
    pub fn new() -> Self {
        Self {
            block: Block::silence(),
            voices: 0,
        }
    }

    /// Adds one voice. Samples past the block's length are ignored; a short voice leaves the
    /// rest of the block as it was.
    pub fn add_i16(&mut self, voice: &[i16], gain: f32) {
        let scale = gain / 32_768.0;
        for (out, s) in self.block.iter_mut().zip(voice.iter().take(BLOCK_LEN)) {
            *out += f32::from(*s) * scale;
        }
        self.voices += 1;
    }

    /// How many voices were added.
    pub fn voices(&self) -> usize {
        self.voices
    }

    /// The mixed block, soft-clipped into -1.0..=1.0; `None` if no voice was added.
    pub fn finish(mut self) -> Option<Block> {
        if self.voices == 0 {
            return None;
        }
        soft_clip(&mut self.block);
        Some(self.block)
    }
}

/// Keeps samples in -1.0..=1.0 without the harsh edge of hard clipping: unchanged up to the
/// knee (0.8), then a tanh curve that meets 1.0 only at infinity.
pub fn soft_clip(samples: &mut [f32]) {
    let room = 1.0 - KNEE;
    for s in samples {
        let magnitude = s.abs();
        if magnitude > KNEE {
            *s = s.signum() * (KNEE + room * ((magnitude - KNEE) / room).tanh());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_voices_no_block() {
        assert!(Mix::new().finish().is_none());
    }

    #[test]
    fn voices_are_summed_with_gain() {
        let mut mix = Mix::new();
        mix.add_i16(&[8192; BLOCK_LEN], 1.0);
        mix.add_i16(&[4096; BLOCK_LEN], 2.0);
        assert_eq!(mix.voices(), 2);
        let block = mix.finish().unwrap();
        // 0.25 + 0.25 = 0.5, under the knee: unchanged.
        assert!(block.iter().all(|s| (*s - 0.5).abs() < 1e-6));
    }

    #[test]
    fn short_and_long_voices() {
        let mut mix = Mix::new();
        mix.add_i16(&[16384; 4], 1.0);
        mix.add_i16(&vec![0; BLOCK_LEN + 10], 1.0);
        let block = mix.finish().unwrap();
        assert_eq!(&block[..4], [0.5; 4]);
        assert!(block[4..].iter().all(|s| *s == 0.0));
    }

    #[test]
    fn loud_sums_are_soft_clipped() {
        let mut samples = [0.5, 0.8, 0.9, 1.5, 4.0, -4.0, -0.3];
        soft_clip(&mut samples);
        assert_eq!(samples[0], 0.5);
        assert_eq!(samples[1], 0.8);
        assert!(samples[2] > 0.8 && samples[2] < 0.9);
        assert!(samples[3] > samples[2] && samples[3] < 1.0);
        assert!(samples[4] > samples[3] && samples[4] <= 1.0);
        assert_eq!(samples[5], -samples[4]);
        assert_eq!(samples[6], -0.3);
    }
}
