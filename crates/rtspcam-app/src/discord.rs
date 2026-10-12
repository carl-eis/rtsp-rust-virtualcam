//! The Discord tab's parts that don't need the window: `discord.json`, logging in at launch
//! (with or without the window), and applying hand edits of the file to the bot.

use std::collections::BTreeMap;
use std::sync::Arc;

use rtspcam_core::discord::{DiscordSettings, DiscordStore, parse_id};
use rtspcam_discord::{DiscordBot, GateSettings, ListenSettings, ServerRules, SessionOptions};

/// The bot and its settings file.
pub(crate) struct Discord {
    pub(crate) store: DiscordStore,
    pub(crate) settings: DiscordSettings,
    /// `discord.json` couldn't be read: the defaults are used and the file is left alone (not
    /// overwritten) until the user fixes it.
    pub(crate) load_error: Option<String>,
    pub(crate) bot: Arc<DiscordBot>,
}

impl Discord {
    /// Reads `discord.json` (next to `config.json`). Never fails: a broken file shows in the
    /// tab and leaves the cameras alone. Starts nothing.
    pub(crate) fn open() -> Option<Self> {
        let store = match DiscordStore::open_default() {
            Ok(store) => store.with_replacer(rtspcam_platform::file_replacer()),
            Err(e) => {
                tracing::warn!(error = %e, "no place for discord.json; the Discord tab is off");
                return None;
            }
        };
        let (settings, load_error) = match store.load() {
            Ok(settings) => {
                for problem in settings.validate() {
                    tracing::warn!(%problem, "invalid Discord setting");
                }
                (settings, None)
            }
            Err(e) => {
                tracing::error!(error = %e, "discord.json is invalid; using the defaults");
                (DiscordSettings::default(), Some(e.to_string()))
            }
        };
        let bot = Arc::new(DiscordBot::new(options(&settings)));
        Some(Self {
            store,
            settings,
            load_error,
            bot,
        })
    }

    /// Logs in if "Log in when RTSP Cam starts" is on.
    pub(crate) fn auto_login(&self) {
        if self.settings.auto_login
            && self.load_error.is_none()
            && !login(&self.bot, &self.settings)
        {
            tracing::warn!("Discord auto-login is on, but no bot token is saved");
        }
    }
}

/// What a new session starts with.
pub(crate) fn options(settings: &DiscordSettings) -> SessionOptions {
    SessionOptions {
        input: settings.input_device.clone(),
        muted: settings.muted,
        gate: gate(settings),
        listen: listen(settings),
        servers: server_rules(settings),
        rejoin: None,
    }
}

pub(crate) fn listen(settings: &DiscordSettings) -> ListenSettings {
    ListenSettings {
        enabled: settings.listen.enabled,
        output: settings.listen.output_device.clone(),
        volume: settings.volume(),
    }
}

/// The servers' auto-join and follow rules, skipping invalid ids.
pub(crate) fn server_rules(settings: &DiscordSettings) -> BTreeMap<u64, ServerRules> {
    settings
        .servers
        .iter()
        .filter_map(|(guild, rules)| {
            let rules = ServerRules {
                auto_join: rules.auto_join(),
                follow: rules.follow(),
            };
            (rules != ServerRules::default()).then_some((parse_id(guild)?, rules))
        })
        .collect()
}

pub(crate) fn gate(settings: &DiscordSettings) -> GateSettings {
    GateSettings {
        enabled: settings.gate.enabled,
        threshold_db: settings.threshold_db(),
    }
}

/// The channel to join again after logging in, if that's wanted.
pub(crate) fn rejoin(settings: &DiscordSettings) -> Option<(u64, u64)> {
    if !settings.rejoin_last_channel {
        return None;
    }
    settings.last_channel.as_ref()?.ids()
}

/// Logs in with the saved token; `false` if there is none.
pub(crate) fn login(bot: &DiscordBot, settings: &DiscordSettings) -> bool {
    match settings.token() {
        Some(token) => {
            bot.login(token.to_owned(), rejoin(settings));
            true
        }
        None => false,
    }
}

/// Makes the bot follow `discord.json` after it was edited by hand.
pub(crate) fn apply_change(bot: &DiscordBot, old: &DiscordSettings, new: &DiscordSettings) {
    if new.muted != old.muted {
        bot.set_muted(new.muted);
    }
    if gate(new) != gate(old) {
        bot.set_gate(gate(new));
    }
    if new.input_device != old.input_device {
        bot.set_input(&new.input_device);
    }
    if listen(new) != listen(old) {
        bot.set_listen(listen(new));
    }
    if server_rules(new) != server_rules(old) {
        bot.set_server_rules(server_rules(new));
    }
    let running = bot.status().borrow().is_running();
    let token_changed = new.token() != old.token();
    if running && token_changed {
        tracing::info!("the bot token changed; logging in again");
        login(bot, new);
    } else if !running && new.auto_login && (!old.auto_login || token_changed) {
        login(bot, new);
    }
}
