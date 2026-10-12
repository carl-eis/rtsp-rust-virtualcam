use rtspcam_audio::Level;

/// What the bot is doing, for the tab. Read it from [`DiscordBot::status`](crate::DiscordBot::status).
#[derive(Debug, Clone, PartialEq, Default)]
pub enum BotStatus {
    /// Logged out (or never logged in). Nothing runs.
    #[default]
    Off,
    LoggingIn,
    Online(Online),
    /// Logging in failed, or the connection to Discord ended for good.
    Failed {
        reason: String,
        hint: String,
    },
}

impl BotStatus {
    pub fn online(&self) -> Option<&Online> {
        match self {
            Self::Online(online) => Some(online),
            _ => None,
        }
    }

    /// Whether the bot is logged in or on its way (so "Log out" makes sense).
    pub fn is_running(&self) -> bool {
        matches!(self, Self::LoggingIn | Self::Online(_))
    }

    pub fn voice(&self) -> Option<&VoiceStatus> {
        self.online().and_then(|o| o.voice.as_ref())
    }
}

/// Logged in.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Online {
    /// The bot's name, as Discord shows it.
    pub user: String,
    /// Adds the bot to a server (opened in the browser).
    pub invite_url: Option<String>,
    /// The servers the bot is in, by name, each with its voice and stage channels.
    pub servers: Vec<Server>,
    pub voice: Option<VoiceStatus>,
    pub input: InputStatus,
    pub muted: bool,
    /// The last thing that went wrong while online (a failed join, a lost device), with what
    /// to do about it. Cleared by the next successful join.
    pub notice: Option<Notice>,
}

impl Online {
    pub fn server(&self, id: u64) -> Option<&Server> {
        self.servers.iter().find(|s| s.id == id)
    }
}

/// A problem to show in the tab.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notice {
    pub text: String,
    pub hint: String,
}

/// A server (Discord's API calls it a guild).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Server {
    pub id: u64,
    pub name: String,
    /// Voice and stage channels, in Discord's order.
    pub channels: Vec<VoiceChannel>,
}

impl Server {
    pub fn channel(&self, id: u64) -> Option<&VoiceChannel> {
        self.channels.iter().find(|c| c.id == id)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VoiceChannel {
    pub id: u64,
    pub name: String,
    pub stage: bool,
    /// Whether the bot's roles let it connect and speak there (`true` when unknown).
    pub can_speak: bool,
}

/// In (or joining) a voice channel.
#[derive(Debug, Clone, PartialEq)]
pub struct VoiceStatus {
    pub guild_id: u64,
    pub channel_id: u64,
    pub state: VoiceState,
    /// How far behind live the queued sound is.
    pub lag_ms: u32,
    /// 20 ms blocks thrown away so far because sending fell behind.
    pub dropped: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VoiceState {
    Joining,
    Connected,
    /// The connection dropped; trying again in `in_secs` (attempt number `attempt`).
    Retrying {
        attempt: u32,
        in_secs: u64,
    },
}

/// The sound source.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum InputStatus {
    /// Not recording (the device opens when the bot joins a channel).
    #[default]
    Closed,
    Open {
        device: String,
    },
    Failed {
        reason: String,
    },
}

/// The sound going out, for the level meter.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Meter {
    pub level: Level,
    /// Whether the gate let the last block through.
    pub gate_open: bool,
}
