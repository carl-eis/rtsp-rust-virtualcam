//! Audio for RTSP Cam's Discord tab, with no Discord code (documentation/19 §3).
//!
//! Everything works in one format, the one Discord's Opus encoder wants: 48 kHz, stereo,
//! interleaved `f32`, in [blocks](Block) of 20 ms (960 samples per channel).
//!
//! - [`devices`]: input devices (microphones, VB-Cable, line in), and where the OS can record
//!   what an output device plays (WASAPI loopback), the output devices as extra sources.
//! - [`Capture`]: opens a device and turns whatever it delivers (any rate, any channel count)
//!   into blocks, through a [`Converter`].
//! - [`BlockQueue`]: a bounded queue between the capture and whoever sends the sound on. Above
//!   a lag limit it drops the oldest blocks, so live sound stays live.
//! - [`Gate`] and [`Level`]: a noise gate and a level meter, per block.
//! - [`Playback`]: plays blocks from a queue on an output device (silence when it runs dry).
//!
//! `cpal` hides the OS (WASAPI, ALSA, CoreAudio), so this crate has no OS-specific code.
//! Opening and listing devices can be slow and, on Windows, sets up COM on the calling thread:
//! call these from a worker thread, never the UI thread.

mod block;
mod capture;
mod convert;
mod device;
mod error;
mod gate;
mod meter;
mod playback;
mod queue;

pub use block::{BLOCK_DURATION, BLOCK_FRAMES, BLOCK_LEN, Block, CHANNELS, SAMPLE_RATE};
pub use capture::{Capture, CaptureInfo};
pub use convert::Converter;
pub use device::{DEFAULT_DEVICE, DeviceInfo, DeviceKind, devices, loopback_supported};
pub use error::AudioError;
pub use gate::{Gate, GateSettings};
pub use meter::{Level, SILENCE_DB, to_db};
pub use playback::Playback;
pub use queue::{BlockQueue, DEFAULT_LAG_LIMIT};
