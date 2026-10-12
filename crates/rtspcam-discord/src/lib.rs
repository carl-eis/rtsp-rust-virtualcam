//! RTSP Cam's Discord voice bot (documentation/19, "Discord Tab").
//!
//! A bot account logs in, joins a voice channel and sends the sound of an audio device into
//! it. [`DiscordBot`] is the whole interface: plain commands in, a [`BotStatus`] out. No
//! serenity or songbird type crosses it, so the UI never depends on them, and moving the bot
//! into a helper process later would only put a pipe behind this handle.
//!
//! - Gateway and REST: `serenity`, with only the `GUILDS` and `GUILD_VOICE_STATES` intents.
//! - Voice: `songbird`, which handles DAVE (Discord's end-to-end encryption, required since
//!   2026-03-01). It is the part that breaks when Discord changes the protocol, so it is used
//!   only in `backend.rs`, and a refused connection reports its close code.
//! - Audio: `rtspcam-audio` (capture, gate, meter, the lag-limited queue).
//!
//! Nothing runs until [`DiscordBot::login`]: no runtime, no connection, no audio device.

mod audio;
mod backend;
mod bot;
mod error;
mod session;
mod status;

pub use bot::{DiscordBot, INVITE_PERMISSIONS, invite_url, list_sources};
pub use error::BotError;
pub use rtspcam_audio::{DEFAULT_DEVICE, DeviceInfo, DeviceKind, GateSettings, Level, SILENCE_DB};
pub use session::SessionOptions;
pub use status::{
    BotStatus, InputStatus, Meter, Notice, Online, Server, VoiceChannel, VoiceState, VoiceStatus,
};
