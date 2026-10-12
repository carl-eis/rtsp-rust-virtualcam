use crate::block::Block;

/// The level reported for digital silence (and the bottom of the meter).
pub const SILENCE_DB: f32 = -90.0;

/// Converts an amplitude (0.0..=1.0) to dBFS, no lower than [`SILENCE_DB`].
pub fn to_db(amplitude: f32) -> f32 {
    if amplitude <= 0.0 {
        return SILENCE_DB;
    }
    (20.0 * amplitude.log10()).max(SILENCE_DB)
}

/// How loud one block is, in dBFS (0 is full scale).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Level {
    pub peak_db: f32,
    pub rms_db: f32,
}

impl Level {
    pub const SILENT: Self = Self {
        peak_db: SILENCE_DB,
        rms_db: SILENCE_DB,
    };

    pub fn of(block: &Block) -> Self {
        Self::of_samples(block)
    }

    pub fn of_samples(samples: &[f32]) -> Self {
        if samples.is_empty() {
            return Self::SILENT;
        }
        let mut peak = 0.0_f32;
        let mut sum = 0.0_f64;
        for s in samples {
            peak = peak.max(s.abs());
            sum += f64::from(*s) * f64::from(*s);
        }
        let rms = (sum / samples.len() as f64).sqrt() as f32;
        Self {
            peak_db: to_db(peak),
            rms_db: to_db(rms),
        }
    }
}

impl Default for Level {
    fn default() -> Self {
        Self::SILENT
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::block::BLOCK_LEN;

    #[test]
    fn full_scale_is_zero_db() {
        let block = Block::from_slice(&[1.0; BLOCK_LEN]).unwrap();
        let level = Level::of(&block);
        assert!(level.peak_db.abs() < 1e-4);
        assert!(level.rms_db.abs() < 1e-4);
    }

    #[test]
    fn half_scale_is_minus_six() {
        let block = Block::from_slice(&[-0.5; BLOCK_LEN]).unwrap();
        assert!((Level::of(&block).peak_db + 6.02).abs() < 0.01);
    }

    #[test]
    fn a_sine_has_rms_three_db_below_peak() {
        let samples: Vec<f32> = (0..BLOCK_LEN)
            .map(|i| (i as f32 / BLOCK_LEN as f32 * std::f32::consts::TAU * 8.0).sin())
            .collect();
        let level = Level::of_samples(&samples);
        assert!(
            (level.peak_db - level.rms_db - 3.01).abs() < 0.05,
            "{level:?}"
        );
    }

    #[test]
    fn silence_is_the_floor() {
        assert_eq!(Level::of(&Block::silence()), Level::SILENT);
        assert_eq!(to_db(1e-9), SILENCE_DB);
    }
}
