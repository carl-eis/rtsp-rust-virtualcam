//! Where the CPU time goes: process-wide time totals per stage of the frame path.
//!
//! Each stage adds the wall-clock time of every call to a counter (two relaxed atomic adds, so
//! it is always on). [`take`] reads and resets the counters; the CLI's `bench` and `view`
//! commands print them once a second, and the app logs them at debug level.
//!
//! The times are wall-clock time on the thread doing the work. For stages that never wait
//! (decode on the CPU, scaling, conversion) that is CPU time. [`Stage::Deliver`] includes
//! waiting for a slow reader, and [`Stage::CopyBack`] includes waiting for the GPU.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// A step on the way from the network to a camera.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Stage {
    /// Decoding, including the decoder's copy of the picture into a `Frame`
    /// (without [`CopyBack`](Self::CopyBack) when that is measured separately).
    Decode,
    /// Hardware decoding only: getting the picture from GPU memory into a `Frame`.
    CopyBack,
    /// Hardware decoding only: shrinking the picture on the GPU before the copy-back.
    GpuScale,
    /// Crop, rotate, flip and the text overlay (once per picture).
    Adjust,
    /// Scaling to a consumer's size (once per consumer per picture).
    Scale,
    /// NV12 to the consumer's pixel format (BGRA, YUYV, I420).
    Convert,
    /// Handing the finished bytes to the camera: a pipe write, or a device write.
    Deliver,
}

impl Stage {
    pub const ALL: [Self; 7] = [
        Self::Decode,
        Self::CopyBack,
        Self::GpuScale,
        Self::Adjust,
        Self::Scale,
        Self::Convert,
        Self::Deliver,
    ];

    pub const fn name(self) -> &'static str {
        match self {
            Self::Decode => "decode",
            Self::CopyBack => "copy-back",
            Self::GpuScale => "gpu-scale",
            Self::Adjust => "adjust",
            Self::Scale => "scale",
            Self::Convert => "convert",
            Self::Deliver => "deliver",
        }
    }

    const fn index(self) -> usize {
        self as usize
    }
}

struct Counter {
    nanos: AtomicU64,
    calls: AtomicU64,
}

#[allow(clippy::declare_interior_mutable_const)]
const ZERO: Counter = Counter {
    nanos: AtomicU64::new(0),
    calls: AtomicU64::new(0),
};

static COUNTERS: [Counter; Stage::ALL.len()] = [ZERO; Stage::ALL.len()];

/// Adds one call of `stage` that took `time`.
pub fn record(stage: Stage, time: Duration) {
    let c = &COUNTERS[stage.index()];
    c.nanos.fetch_add(
        u64::try_from(time.as_nanos()).unwrap_or(u64::MAX),
        Ordering::Relaxed,
    );
    c.calls.fetch_add(1, Ordering::Relaxed);
}

/// Times the rest of the enclosing scope as one call of `stage`.
#[must_use = "the time is recorded when the guard is dropped"]
pub fn time(stage: Stage) -> Timer {
    Timer {
        stage,
        start: Instant::now(),
    }
}

/// See [`time`].
#[derive(Debug)]
pub struct Timer {
    stage: Stage,
    start: Instant,
}

impl Drop for Timer {
    fn drop(&mut self) {
        record(self.stage, self.start.elapsed());
    }
}

/// One stage's totals since the last [`take`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StageTotal {
    pub stage: Stage,
    pub time: Duration,
    pub calls: u64,
}

impl StageTotal {
    /// Average time per call.
    pub fn per_call(&self) -> Duration {
        if self.calls == 0 {
            Duration::ZERO
        } else {
            self.time / u32::try_from(self.calls).unwrap_or(u32::MAX)
        }
    }
}

/// Every stage's totals since the last call, and resets them.
pub fn take() -> Vec<StageTotal> {
    Stage::ALL
        .iter()
        .map(|&stage| {
            let c = &COUNTERS[stage.index()];
            StageTotal {
                stage,
                time: Duration::from_nanos(c.nanos.swap(0, Ordering::Relaxed)),
                calls: c.calls.swap(0, Ordering::Relaxed),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_and_resets() {
        // Other tests in this binary don't use the profiler, so the counters are ours.
        let _ = take();
        record(Stage::Scale, Duration::from_millis(2));
        record(Stage::Scale, Duration::from_millis(4));
        {
            let _t = time(Stage::Convert);
        }
        let totals = take();
        let scale = totals.iter().find(|t| t.stage == Stage::Scale).unwrap();
        assert_eq!((scale.time, scale.calls), (Duration::from_millis(6), 2));
        assert_eq!(scale.per_call(), Duration::from_millis(3));
        let convert = totals.iter().find(|t| t.stage == Stage::Convert).unwrap();
        assert_eq!(convert.calls, 1);
        assert!(take().iter().all(|t| t.calls == 0));
    }
}
