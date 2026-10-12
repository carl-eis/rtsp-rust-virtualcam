use crate::block::{BLOCK_DURATION, Block, CHANNELS, SAMPLE_RATE};
use crate::meter::Level;

/// How long the gate stays open after the sound drops below the threshold, so word endings
/// and short pauses aren't cut.
const HOLD: std::time::Duration = std::time::Duration::from_millis(200);
/// How long opening and closing take, so the gate doesn't click.
const FADE_FRAMES: usize = SAMPLE_RATE as usize * 5 / 1000;

/// The gate's settings, as stored in `discord.json`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GateSettings {
    pub enabled: bool,
    /// Sound quieter than this (block peak, dBFS) is not sent.
    pub threshold_db: f32,
}

impl Default for GateSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            threshold_db: -50.0,
        }
    }
}

/// A noise gate on 20 ms blocks: blocks whose peak is below the threshold become silence once
/// the hold time has passed, with a short fade instead of a hard cut.
///
/// (The Java bot zeroes each *sample* below its threshold, which distorts quiet sound instead
/// of gating it.)
#[derive(Debug, Clone)]
pub struct Gate {
    settings: GateSettings,
    /// Blocks left before the gate closes.
    hold_left: u32,
    /// Current gain, 0.0 (closed) to 1.0 (open), moved towards the target one frame at a time.
    gain: f32,
}

impl Gate {
    pub fn new(settings: GateSettings) -> Self {
        Self {
            settings,
            hold_left: 0,
            gain: if settings.enabled { 0.0 } else { 1.0 },
        }
    }

    pub fn settings(&self) -> GateSettings {
        self.settings
    }

    pub fn set_settings(&mut self, settings: GateSettings) {
        self.settings = settings;
    }

    /// Gates `block` in place, given its level. Returns whether any sound is left in it.
    pub fn process(&mut self, block: &mut Block, level: Level) -> bool {
        let hold_blocks = (HOLD.as_millis() / BLOCK_DURATION.as_millis()) as u32;
        let open = if !self.settings.enabled {
            true
        } else if level.peak_db >= self.settings.threshold_db {
            self.hold_left = hold_blocks;
            true
        } else if self.hold_left > 0 {
            self.hold_left -= 1;
            true
        } else {
            false
        };
        let target = if open { 1.0 } else { 0.0 };
        if self.gain == target {
            if !open {
                block.fill(0.0);
            }
            return open;
        }
        let step = 1.0 / FADE_FRAMES as f32;
        for frame in block.as_chunks_mut::<CHANNELS>().0 {
            self.gain = if target > self.gain {
                (self.gain + step).min(target)
            } else {
                (self.gain - step).max(target)
            };
            for sample in frame {
                *sample *= self.gain;
            }
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::block::BLOCK_LEN;

    fn loud() -> Block {
        Block::from_slice(&[0.5; BLOCK_LEN]).unwrap()
    }

    fn quiet() -> Block {
        // About -66 dBFS.
        Block::from_slice(&[0.0005; BLOCK_LEN]).unwrap()
    }

    fn run(gate: &mut Gate, mut block: Block) -> (bool, Block) {
        let level = Level::of(&block);
        let open = gate.process(&mut block, level);
        (open, block)
    }

    #[test]
    fn quiet_sound_is_silenced() {
        let mut gate = Gate::new(GateSettings::default());
        let (open, block) = run(&mut gate, quiet());
        assert!(!open);
        assert!(block.is_silent());
    }

    #[test]
    fn loud_sound_opens_with_a_fade() {
        let mut gate = Gate::new(GateSettings::default());
        let (open, block) = run(&mut gate, loud());
        assert!(open);
        // Starts near zero, reaches full level after 5 ms.
        assert!(block[0] < 0.01);
        assert_eq!(block[FADE_FRAMES * CHANNELS], 0.5);
        assert_eq!(*block.last().unwrap(), 0.5);
        // Fully open now: untouched.
        let (_, block) = run(&mut gate, loud());
        assert_eq!(block, loud());
    }

    #[test]
    fn stays_open_for_the_hold_time_then_fades_out() {
        let mut gate = Gate::new(GateSettings::default());
        run(&mut gate, loud());
        // 200 ms = 10 blocks of hold, untouched.
        for _ in 0..10 {
            let (open, block) = run(&mut gate, quiet());
            assert!(open);
            assert_eq!(block, quiet());
        }
        // Then a fade to zero within the block, then silence.
        let (open, block) = run(&mut gate, quiet());
        assert!(open, "the fade-out still carries sound");
        assert!(block[0] > 0.0);
        assert_eq!(*block.last().unwrap(), 0.0);
        let (open, block) = run(&mut gate, quiet());
        assert!(!open);
        assert!(block.is_silent());
    }

    #[test]
    fn disabled_gate_passes_everything() {
        let mut gate = Gate::new(GateSettings {
            enabled: false,
            threshold_db: -50.0,
        });
        let (open, block) = run(&mut gate, quiet());
        assert!(open);
        assert_eq!(block, quiet());
    }

    #[test]
    fn threshold_can_change_live() {
        let mut gate = Gate::new(GateSettings::default());
        gate.set_settings(GateSettings {
            enabled: true,
            threshold_db: -80.0,
        });
        let (open, _) = run(&mut gate, quiet());
        assert!(open);
    }
}
