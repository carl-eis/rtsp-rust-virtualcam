use std::ops::{Deref, DerefMut};
use std::time::Duration;

/// Samples per second, per channel.
pub const SAMPLE_RATE: u32 = 48_000;
/// Stereo.
pub const CHANNELS: usize = 2;
/// Samples per channel in one block (20 ms at 48 kHz).
pub const BLOCK_FRAMES: usize = 960;
/// Samples in one block, both channels.
pub const BLOCK_LEN: usize = BLOCK_FRAMES * CHANNELS;
/// How long one block lasts.
pub const BLOCK_DURATION: Duration = Duration::from_millis(20);

/// 20 ms of 48 kHz stereo sound, interleaved (left, right, left, ...), in -1.0..=1.0.
#[derive(Clone, PartialEq)]
pub struct Block(Box<[f32; BLOCK_LEN]>);

impl Block {
    pub fn silence() -> Self {
        Self(Box::new([0.0; BLOCK_LEN]))
    }

    /// A block from exactly [`BLOCK_LEN`] samples; `None` for any other length.
    pub fn from_slice(samples: &[f32]) -> Option<Self> {
        let array: [f32; BLOCK_LEN] = samples.try_into().ok()?;
        Some(Self(Box::new(array)))
    }

    /// Whether every sample is zero.
    pub fn is_silent(&self) -> bool {
        self.0.iter().all(|s| *s == 0.0)
    }
}

impl Default for Block {
    fn default() -> Self {
        Self::silence()
    }
}

impl Deref for Block {
    type Target = [f32];

    fn deref(&self) -> &[f32] {
        &self.0[..]
    }
}

impl DerefMut for Block {
    fn deref_mut(&mut self) -> &mut [f32] {
        &mut self.0[..]
    }
}

impl std::fmt::Debug for Block {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Block")
            .field("silent", &self.is_silent())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_block_is_twenty_milliseconds() {
        let per_second = SAMPLE_RATE as usize * CHANNELS;
        assert_eq!(BLOCK_LEN * 50, per_second);
        assert_eq!(BLOCK_DURATION * 50, Duration::from_secs(1));
    }

    #[test]
    fn from_slice_wants_the_exact_length() {
        assert!(Block::from_slice(&[0.0; BLOCK_LEN]).is_some());
        assert!(Block::from_slice(&[0.0; BLOCK_LEN - 1]).is_none());
        assert!(Block::silence().is_silent());
    }
}
