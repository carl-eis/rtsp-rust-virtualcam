//! The send path: capture → gate → queue → the voice connection.

use std::io::{self, Read, Seek, SeekFrom};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use rtspcam_audio::{AudioError, BLOCK_LEN, Block, BlockQueue, Capture, Gate, GateSettings, Level};

use crate::status::Meter;

/// Settings and readings shared between the bot and the capture callback, through atomics: the
/// callback runs on the audio system's thread and must never wait on a lock.
#[derive(Debug)]
pub(crate) struct AudioShared {
    pub(crate) queue: Arc<BlockQueue>,
    muted: AtomicBool,
    gate_enabled: AtomicBool,
    threshold_db: AtomicU32,
    peak_db: AtomicU32,
    rms_db: AtomicU32,
    gate_open: AtomicBool,
}

impl AudioShared {
    pub(crate) fn new(muted: bool, gate: GateSettings) -> Self {
        let silent = Level::SILENT;
        Self {
            queue: Arc::new(BlockQueue::default()),
            muted: AtomicBool::new(muted),
            gate_enabled: AtomicBool::new(gate.enabled),
            threshold_db: AtomicU32::new(gate.threshold_db.to_bits()),
            peak_db: AtomicU32::new(silent.peak_db.to_bits()),
            rms_db: AtomicU32::new(silent.rms_db.to_bits()),
            gate_open: AtomicBool::new(false),
        }
    }

    pub(crate) fn set_muted(&self, muted: bool) {
        let was = self.muted.swap(muted, Ordering::Relaxed);
        if was && !muted {
            // Don't send sound from before the unmute.
            self.queue.clear();
        }
    }

    pub(crate) fn set_gate(&self, gate: GateSettings) {
        self.gate_enabled.store(gate.enabled, Ordering::Relaxed);
        self.threshold_db
            .store(gate.threshold_db.to_bits(), Ordering::Relaxed);
    }

    fn gate(&self) -> GateSettings {
        GateSettings {
            enabled: self.gate_enabled.load(Ordering::Relaxed),
            threshold_db: f32::from_bits(self.threshold_db.load(Ordering::Relaxed)),
        }
    }

    pub(crate) fn meter(&self) -> Meter {
        Meter {
            level: Level {
                peak_db: f32::from_bits(self.peak_db.load(Ordering::Relaxed)),
                rms_db: f32::from_bits(self.rms_db.load(Ordering::Relaxed)),
            },
            gate_open: self.gate_open.load(Ordering::Relaxed),
        }
    }

    /// Back to silence on the meter (when the capture closes).
    pub(crate) fn reset_meter(&self) {
        self.record(Level::SILENT, false);
        self.queue.clear();
    }

    fn record(&self, level: Level, open: bool) {
        self.peak_db
            .store(level.peak_db.to_bits(), Ordering::Relaxed);
        self.rms_db.store(level.rms_db.to_bits(), Ordering::Relaxed);
        self.gate_open.store(open, Ordering::Relaxed);
    }

    /// What the capture does with each block: measure, gate, and queue it unless muted.
    pub(crate) fn sink(self: &Arc<Self>) -> impl FnMut(Block) + Send + 'static {
        let shared = self.clone();
        let mut gate = Gate::new(shared.gate());
        move |mut block| {
            let settings = shared.gate();
            if settings != gate.settings() {
                gate.set_settings(settings);
            }
            let level = Level::of(&block);
            let open = gate.process(&mut block, level);
            shared.record(level, open);
            if !shared.muted.load(Ordering::Relaxed) {
                shared.queue.push(block);
            }
        }
    }
}

/// Opens sound sources. The real one uses `rtspcam-audio`; tests use a fake.
pub(crate) trait Inputs: Send + Sync + 'static {
    /// Starts recording from device `id` into `sink`; returns what keeps it running (dropping
    /// it stops the recording) and the device's name. Slow: called on a blocking thread.
    fn open(
        &self,
        id: &str,
        sink: Box<dyn FnMut(Block) + Send>,
        on_error: Box<dyn FnMut(AudioError) + Send>,
    ) -> Result<(Box<dyn Send>, String), AudioError>;
}

/// Audio devices through `cpal`.
#[derive(Debug, Default)]
pub(crate) struct DeviceInputs;

impl Inputs for DeviceInputs {
    fn open(
        &self,
        id: &str,
        sink: Box<dyn FnMut(Block) + Send>,
        on_error: Box<dyn FnMut(AudioError) + Send>,
    ) -> Result<(Box<dyn Send>, String), AudioError> {
        let capture = Capture::open(id, sink, on_error)?;
        let name = capture.info().device_name.clone();
        Ok((Box::new(capture), name))
    }
}

/// Reads the queue as a byte stream of interleaved little-endian `f32` (48 kHz stereo), the
/// raw format songbird's `RawAdapter` takes.
///
/// It never runs dry and never blocks: when the queue is empty it returns a short stretch of
/// silence, because 0 bytes would end the track and waiting would stall the voice mixer. The
/// silence is short (2.5 ms) so that sound arriving just after an underrun isn't delayed by
/// much.
pub(crate) struct QueueReader {
    queue: Arc<BlockQueue>,
    /// The block being read and the byte position in it.
    current: Vec<u8>,
    pos: usize,
}

/// 2.5 ms of silence, in bytes (a whole number of stereo frames).
const SILENCE_BYTES: usize = BLOCK_LEN / 8 * 4;

impl QueueReader {
    pub(crate) fn new(queue: Arc<BlockQueue>) -> Self {
        Self {
            queue,
            current: Vec::with_capacity(BLOCK_LEN * 4),
            pos: 0,
        }
    }

    fn refill(&mut self) {
        self.current.clear();
        self.pos = 0;
        match self.queue.pop() {
            Some(block) => {
                for sample in block.iter() {
                    self.current.extend_from_slice(&sample.to_le_bytes());
                }
            }
            None => self.current.resize(SILENCE_BYTES, 0),
        }
    }
}

impl Read for QueueReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        if self.pos >= self.current.len() {
            self.refill();
        }
        let n = buf.len().min(self.current.len() - self.pos);
        buf[..n].copy_from_slice(&self.current[self.pos..self.pos + n]);
        self.pos += n;
        Ok(n)
    }
}

impl Seek for QueueReader {
    fn seek(&mut self, _: SeekFrom) -> io::Result<u64> {
        Err(io::ErrorKind::Unsupported.into())
    }
}

impl songbird::input::core::io::MediaSource for QueueReader {
    fn is_seekable(&self) -> bool {
        false
    }

    fn byte_len(&self) -> Option<u64> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn block_of(value: f32) -> Block {
        Block::from_slice(&[value; BLOCK_LEN]).unwrap()
    }

    fn read_f32s(reader: &mut QueueReader, count: usize) -> Vec<f32> {
        let mut bytes = vec![0_u8; count * 4];
        let mut filled = 0;
        while filled < bytes.len() {
            let n = reader.read(&mut bytes[filled..]).unwrap();
            assert!(n > 0, "the reader must never end the stream");
            filled += n;
        }
        bytes
            .as_chunks::<4>()
            .0
            .iter()
            .map(|b| f32::from_le_bytes(*b))
            .collect()
    }

    #[test]
    fn reads_queued_blocks_then_silence() {
        let queue = Arc::new(BlockQueue::default());
        queue.push(block_of(0.25));
        let mut reader = QueueReader::new(queue.clone());
        let samples = read_f32s(&mut reader, BLOCK_LEN);
        assert!(samples.iter().all(|s| *s == 0.25));
        // Empty: silence, and the stream goes on.
        let samples = read_f32s(&mut reader, BLOCK_LEN * 3);
        assert!(samples.iter().all(|s| *s == 0.0));
        // Sound that arrives is picked up within 2.5 ms.
        queue.push(block_of(0.5));
        let samples = read_f32s(&mut reader, BLOCK_LEN);
        let first = samples.iter().position(|s| *s == 0.5).unwrap();
        assert!(first <= BLOCK_LEN / 8, "{first}");
    }

    #[test]
    fn odd_sized_reads_keep_samples_aligned() {
        let queue = Arc::new(BlockQueue::default());
        queue.push(block_of(0.75));
        queue.push(block_of(-0.75));
        let mut reader = QueueReader::new(queue);
        let mut bytes = Vec::new();
        let mut buf = [0_u8; 7];
        while bytes.len() < BLOCK_LEN * 8 {
            let n = reader.read(&mut buf).unwrap();
            bytes.extend_from_slice(&buf[..n]);
        }
        let samples: Vec<f32> = bytes[..BLOCK_LEN * 8]
            .as_chunks::<4>()
            .0
            .iter()
            .map(|b| f32::from_le_bytes(*b))
            .collect();
        assert!(samples[..BLOCK_LEN].iter().all(|s| *s == 0.75));
        assert!(samples[BLOCK_LEN..].iter().all(|s| *s == -0.75));
    }

    #[test]
    fn the_sink_gates_meters_and_respects_mute() {
        let shared = Arc::new(AudioShared::new(true, GateSettings::default()));
        let mut sink = shared.sink();
        sink(block_of(0.5));
        assert!(shared.queue.is_empty(), "muted: nothing is queued");
        let meter = shared.meter();
        assert!((meter.level.peak_db + 6.02).abs() < 0.01);
        assert!(meter.gate_open);

        shared.set_muted(false);
        sink(block_of(0.5));
        assert_eq!(shared.queue.len(), 1);

        // Below the threshold, past the hold time: silence is queued, the gate shows closed.
        shared.set_gate(GateSettings {
            enabled: true,
            threshold_db: -3.0,
        });
        for _ in 0..12 {
            sink(block_of(0.5));
        }
        assert!(!shared.meter().gate_open);
        let last = std::iter::from_fn(|| shared.queue.pop()).last().unwrap();
        assert!(last.is_silent());
    }

    #[test]
    fn unmuting_drops_stale_sound() {
        let shared = AudioShared::new(false, GateSettings::default());
        shared.queue.push(block_of(0.5));
        shared.set_muted(true);
        shared.set_muted(false);
        assert!(shared.queue.is_empty());
    }
}
