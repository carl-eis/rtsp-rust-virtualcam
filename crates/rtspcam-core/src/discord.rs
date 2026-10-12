//! `discord.json`: the Discord tab's settings (documentation/19 §4 D3).
//!
//! A file of its own next to `config.json`, so a broken Discord setting can't stop the streams
//! from loading, `config.json`'s schema doesn't change, and stream import/export never sees
//! the bot token. It is read and written by the same [`JsonStore`] code (atomic save, `.bak`,
//! watcher).

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::config::{Document, JsonStore, Migration};
use crate::error::ConfigError;
use crate::secret::Secret;

pub use crate::constants::DISCORD_FILE_NAME;

/// Schema version of `discord.json` written by this build.
pub const DISCORD_VERSION: u64 = 1;

/// The quietest gate threshold that can be set, in dBFS (the meter's floor).
pub const MIN_THRESHOLD_DB: f32 = -90.0;

/// The device id that follows the system's default input.
pub const DEFAULT_INPUT: &str = "default";

/// Reads and writes `discord.json`.
pub type DiscordStore = JsonStore<DiscordSettings>;

impl DiscordStore {
    /// A store for the user's `discord.json` (see [`crate::paths::discord_file`]).
    pub fn open_default() -> Result<Self, ConfigError> {
        Ok(Self::new(crate::paths::discord_file()?))
    }
}

/// Root of `discord.json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DiscordSettings {
    pub version: u64,
    /// The bot's token, encrypted like stream passwords. A full credential: never logged.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token: Option<Secret>,
    /// Log in when RTSP Cam starts (also with `--headless`).
    pub auto_login: bool,
    /// The sound source: [`DEFAULT_INPUT`], or an `input:`/`output:` id from `rtspcam-audio`.
    pub input_device: String,
    /// Nothing is sent while muted. Starts `true`, so a new bot never broadcasts before the
    /// user means it to.
    pub muted: bool,
    pub gate: GateConfig,
    /// The channel the bot was last in.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_channel: Option<ChannelRef>,
    /// Join [`last_channel`](Self::last_channel) again after logging in.
    pub rejoin_last_channel: bool,
    /// Fields this build doesn't know about, kept so saving doesn't drop them.
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl Default for DiscordSettings {
    fn default() -> Self {
        Self {
            version: DISCORD_VERSION,
            token: None,
            auto_login: false,
            input_device: DEFAULT_INPUT.to_owned(),
            muted: true,
            gate: GateConfig::default(),
            last_channel: None,
            rejoin_last_channel: true,
            extra: Map::new(),
        }
    }
}

impl Document for DiscordSettings {
    const CURRENT_VERSION: u64 = DISCORD_VERSION;
    const MIGRATIONS: &'static [Migration] = &[];
}

const _: () =
    assert!(<DiscordSettings as Document>::MIGRATIONS.len() as u64 + 1 == DISCORD_VERSION);

/// The noise gate.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct GateConfig {
    pub enabled: bool,
    /// Sound quieter than this (dBFS, [`MIN_THRESHOLD_DB`] to 0) is not sent.
    pub threshold_db: f32,
}

impl Default for GateConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            threshold_db: -50.0,
        }
    }
}

/// A voice channel in a server, by id. Ids are strings, as Discord writes them (they don't
/// fit in a JSON number for every reader).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChannelRef {
    pub guild_id: String,
    pub channel_id: String,
}

impl ChannelRef {
    pub fn new(guild_id: u64, channel_id: u64) -> Self {
        Self {
            guild_id: guild_id.to_string(),
            channel_id: channel_id.to_string(),
        }
    }

    /// Both ids as numbers, if they are valid.
    pub fn ids(&self) -> Option<(u64, u64)> {
        Some((parse_id(&self.guild_id)?, parse_id(&self.channel_id)?))
    }
}

fn parse_id(id: &str) -> Option<u64> {
    id.trim().parse().ok().filter(|n| *n != 0)
}

/// Something wrong in `discord.json`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DiscordProblem {
    #[error("the gate threshold must be between -90 and 0 dB")]
    ThresholdOutOfRange,
    #[error("the last channel's ids must be numbers")]
    InvalidChannel,
    #[error("the input device must not be empty")]
    NoInput,
    #[error(
        "the saved bot token can't be decrypted (was the file copied from another user or PC?); paste it again"
    )]
    TokenLocked,
}

impl DiscordSettings {
    /// Checks the settings. An empty result means valid.
    pub fn validate(&self) -> Vec<DiscordProblem> {
        let mut problems = Vec::new();
        let t = self.gate.threshold_db;
        if !t.is_finite() || !(MIN_THRESHOLD_DB..=0.0).contains(&t) {
            problems.push(DiscordProblem::ThresholdOutOfRange);
        }
        if self
            .last_channel
            .as_ref()
            .is_some_and(|c| c.ids().is_none())
        {
            problems.push(DiscordProblem::InvalidChannel);
        }
        if self.input_device.trim().is_empty() {
            problems.push(DiscordProblem::NoInput);
        }
        if self
            .token
            .as_ref()
            .is_some_and(|t| t.locked_reason().is_some())
        {
            problems.push(DiscordProblem::TokenLocked);
        }
        problems
    }

    /// The token, if one is saved and readable.
    pub fn token(&self) -> Option<&str> {
        self.token
            .as_ref()
            .and_then(Secret::expose)
            .filter(|t| !t.trim().is_empty())
    }

    /// The gate threshold, clamped to the allowed range.
    pub fn threshold_db(&self) -> f32 {
        let t = self.gate.threshold_db;
        if t.is_finite() {
            t.clamp(MIN_THRESHOLD_DB, 0.0)
        } else {
            GateConfig::default().threshold_db
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::secret::test_store;

    fn store() -> (tempfile::TempDir, DiscordStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = DiscordStore::new(dir.path().join(DISCORD_FILE_NAME));
        (dir, store)
    }

    #[test]
    fn a_missing_file_gives_safe_defaults() {
        let (_dir, store) = store();
        let settings = store.load().unwrap();
        assert!(settings.muted, "a new bot starts muted");
        assert!(!settings.auto_login);
        assert_eq!(settings.input_device, DEFAULT_INPUT);
        assert!(settings.token.is_none());
        assert!(settings.validate().is_empty());
    }

    #[test]
    fn the_token_is_stored_encrypted() {
        test_store::install();
        let (_dir, store) = store();
        let settings = DiscordSettings {
            token: Some(Secret::new("MTIz.secret-token")),
            last_channel: Some(ChannelRef::new(1234, 5678)),
            ..DiscordSettings::default()
        };
        store.save(&settings).unwrap();
        let text = std::fs::read_to_string(store.path()).unwrap();
        assert!(!text.contains("secret-token"), "{text}");
        assert!(text.contains("\"guild_id\": \"1234\""), "{text}");
        let back = store.load().unwrap();
        assert_eq!(back.token(), Some("MTIz.secret-token"));
        assert_eq!(back, settings);
    }

    #[test]
    fn hand_written_files_load_and_keep_unknown_fields() {
        let (_dir, store) = store();
        std::fs::write(
            store.path(),
            r#"{ "muted": false, "gate": { "threshold_db": -40 }, "future": 1 }"#,
        )
        .unwrap();
        let settings = store.load().unwrap();
        assert!(!settings.muted);
        assert!(settings.gate.enabled);
        assert_eq!(settings.gate.threshold_db, -40.0);
        assert_eq!(settings.extra["future"], 1);
        store.save(&settings).unwrap();
        assert!(
            std::fs::read_to_string(store.path())
                .unwrap()
                .contains("future")
        );
    }

    #[test]
    fn invalid_json_is_an_error_not_a_panic() {
        let (_dir, store) = store();
        std::fs::write(store.path(), "{ nope").unwrap();
        assert!(matches!(store.load(), Err(ConfigError::Parse { .. })));
    }

    #[test]
    fn validation() {
        let mut s = DiscordSettings::default();
        s.gate.threshold_db = 3.0;
        s.last_channel = Some(ChannelRef {
            guild_id: "abc".into(),
            channel_id: "1".into(),
        });
        s.input_device = " ".into();
        assert_eq!(
            s.validate(),
            [
                DiscordProblem::ThresholdOutOfRange,
                DiscordProblem::InvalidChannel,
                DiscordProblem::NoInput
            ]
        );
        assert_eq!(s.threshold_db(), 0.0);
        s.gate.threshold_db = f32::NAN;
        assert_eq!(s.threshold_db(), -50.0);
    }

    #[test]
    fn channel_ids() {
        assert_eq!(ChannelRef::new(7, 9).ids(), Some((7, 9)));
        let zero = ChannelRef {
            guild_id: "0".into(),
            channel_id: "9".into(),
        };
        assert_eq!(zero.ids(), None);
    }

    #[test]
    fn an_empty_token_counts_as_none() {
        let s = DiscordSettings {
            token: Some(Secret::new("  ")),
            ..DiscordSettings::default()
        };
        assert_eq!(s.token(), None);
    }
}
