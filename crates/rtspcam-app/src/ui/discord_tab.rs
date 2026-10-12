//! The Discord tab (documentation/19 §4 D4): fills `DiscordTab`'s properties from the bot's
//! status and turns its callbacks into bot commands and `discord.json` saves.
//!
//! Opening the tab starts nothing. While the bot runs, a timer reads its status and the level
//! meter (at most 20 times a second); it stops again once the bot is off. Audio devices are
//! listed by the bot's worker thread, never here, on "Log in" or "Refresh".

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use rtspcam_core::Secret;
use rtspcam_core::config::ConfigWatcher;
use rtspcam_core::discord::{ChannelRef, DEFAULT_INPUT, DiscordSettings, DiscordStore};
use rtspcam_discord::{
    BotStatus, DeviceInfo, DeviceKind, DiscordBot, InputStatus, Online, SILENCE_DB, Server,
    VoiceState, list_sources,
};
use slint::{ComponentHandle as _, ModelRc, SharedString, Timer, TimerMode, VecModel};
use tokio::sync::watch;

use super::app::{App, with_app};
use super::generated::{DiscordTab, MainWindow};
use super::message;
use crate::discord::{self, Discord};

/// The meter and status refresh rate while the bot runs.
const POLL: Duration = Duration::from_millis(50);
/// Slider moves are saved once they stop for this long.
const SAVE_DELAY: Duration = Duration::from_millis(500);
/// The bot setup steps.
const SETUP_HELP_URL: &str =
    "https://github.com/carl-eis/rtsp-rust-virtualcam/blob/master/documentation/manual/discord.md";

pub(crate) struct DiscordUi {
    store: DiscordStore,
    settings: RefCell<DiscordSettings>,
    load_error: Option<String>,
    pub(crate) bot: Arc<DiscordBot>,
    status: RefCell<watch::Receiver<BotStatus>>,
    /// The servers as shown in the picker, and the picked server and channel ids.
    servers: RefCell<Vec<Server>>,
    picked: Cell<Option<(u64, Option<u64>)>>,
    /// The source picker's entries, in order.
    devices: RefCell<Vec<DeviceInfo>>,
    poll: Timer,
    save_later: Timer,
    /// `discord.json` read again after a hand edit.
    external: Arc<Mutex<Option<DiscordSettings>>>,
    _watcher: Option<ConfigWatcher>,
}

impl DiscordUi {
    pub(crate) fn new(discord: Discord) -> Self {
        let Discord {
            store,
            settings,
            load_error,
            bot,
        } = discord;
        let external = Arc::new(Mutex::new(None));
        let watcher = load_error.is_none().then(|| {
            let external = external.clone();
            store
                .watch(move |result| match result {
                    Ok(settings) => {
                        *external.lock().unwrap_or_else(PoisonError::into_inner) = Some(settings);
                    }
                    Err(e) => tracing::error!(error = %e, "discord.json is invalid; ignoring it"),
                })
                .map_err(|e| tracing::warn!(error = %e, "not watching discord.json"))
                .ok()
        });
        let status = bot.status();
        let picked = settings
            .last_channel
            .as_ref()
            .and_then(ChannelRef::ids)
            .map(|(g, c)| (g, Some(c)));
        Self {
            store,
            settings: RefCell::new(settings),
            load_error,
            bot,
            status: RefCell::new(status),
            servers: RefCell::default(),
            picked: Cell::new(picked),
            devices: RefCell::default(),
            poll: Timer::default(),
            save_later: Timer::default(),
            external,
            _watcher: watcher.flatten(),
        }
    }

    /// Writes `discord.json`, unless it couldn't be read (then it is left for the user to fix).
    fn save(&self, app: &Rc<App>) {
        if self.load_error.is_some() {
            return;
        }
        if let Err(e) = self.store.save(&self.settings.borrow()) {
            message::inform(
                app,
                "Discord",
                &format!("Could not save the Discord settings: {e}"),
            );
        }
    }

    pub(crate) fn in_voice(&self) -> bool {
        self.status.borrow().borrow().voice().is_some()
    }

    pub(crate) fn muted(&self) -> bool {
        self.settings.borrow().muted
    }
}

fn with_tab(app: &Rc<App>, f: impl FnOnce(&DiscordUi, &DiscordTab<'_>)) {
    let Some(ui) = app.ui() else { return };
    let tab = ui.global::<DiscordTab>();
    if let Some(d) = app.discord.as_ref() {
        f(d, &tab);
    }
}

fn strings(items: impl IntoIterator<Item = String>) -> ModelRc<SharedString> {
    ModelRc::new(VecModel::from(
        items
            .into_iter()
            .map(SharedString::from)
            .collect::<Vec<_>>(),
    ))
}

/// Fills the tab once, at startup.
pub(crate) fn init(app: &Rc<App>, ui: &MainWindow) {
    let tab = ui.global::<DiscordTab>();
    let Some(d) = app.discord.as_ref() else {
        tab.set_available(false);
        return;
    };
    tab.set_available(true);
    tab.set_settings_error(d.load_error.clone().unwrap_or_default().into());
    show_settings(d, &tab);
    show_devices(d, &tab);
    show_status(d, &tab, &d.status.borrow().borrow().clone());
    if d.bot.status().borrow().is_running() {
        start_polling(app);
    }
}

/// The saved settings into the fields.
fn show_settings(d: &DiscordUi, tab: &DiscordTab<'_>) {
    let s = d.settings.borrow();
    tab.set_has_token(s.token().is_some());
    tab.set_auto_login(s.auto_login);
    tab.set_muted(s.muted);
    tab.set_gate_enabled(s.gate.enabled);
    tab.set_threshold(s.threshold_db());
}

/// The source picker: the system default, the saved device until the list is known, then
/// whatever was listed.
fn show_devices(d: &DiscordUi, tab: &DiscordTab<'_>) {
    let saved = d.settings.borrow().input_device.clone();
    let mut devices = d.devices.borrow_mut();
    if devices.is_empty() {
        devices.push(default_entry());
    }
    if !devices.iter().any(|dev| dev.id == saved) {
        devices.push(DeviceInfo {
            id: saved.clone(),
            name: "The saved source (Refresh to list the devices)".into(),
            kind: DeviceKind::Input,
            is_default: false,
        });
    }
    tab.set_devices(strings(devices.iter().map(device_label)));
    tab.set_device(devices.iter().position(|dev| dev.id == saved).unwrap_or(0) as i32);
}

fn default_entry() -> DeviceInfo {
    DeviceInfo {
        id: DEFAULT_INPUT.into(),
        name: "System default input".into(),
        kind: DeviceKind::Input,
        is_default: false,
    }
}

fn device_label(device: &DeviceInfo) -> String {
    let default = if device.is_default { " (default)" } else { "" };
    match device.kind {
        DeviceKind::Input => format!("{}{default}", device.name),
        DeviceKind::Output => format!("{}{default} - what the PC plays", device.name),
    }
}

fn refresh_devices(app: &Rc<App>) {
    list_sources(|result| {
        let _ = slint::invoke_from_event_loop(move || {
            with_app(|app| match result {
                Ok(list) => with_tab(app, |d, tab| {
                    *d.devices.borrow_mut() =
                        std::iter::once(default_entry()).chain(list).collect();
                    show_devices(d, tab);
                }),
                Err(e) => message::inform(
                    app,
                    "Discord",
                    &format!("Could not list the audio devices: {e}"),
                ),
            });
        });
    });
    let _ = app;
}

fn start_polling(app: &Rc<App>) {
    let Some(d) = app.discord.as_ref() else {
        return;
    };
    if d.poll.running() {
        return;
    }
    let me = Rc::downgrade(app);
    d.poll.start(TimerMode::Repeated, POLL, move || {
        if let Some(app) = me.upgrade() {
            poll(&app);
        }
    });
}

/// Brings the status and meter up to date; stops polling once the bot is off.
fn poll(app: &Rc<App>) {
    let mut keep = true;
    with_tab(app, |d, tab| {
        let changed = d.status.borrow().has_changed().unwrap_or(false);
        let status = d.status.borrow_mut().borrow_and_update().clone();
        if changed {
            show_status(d, tab, &status);
        }
        match d.bot.meter() {
            Some(meter) => {
                tab.set_metering(true);
                tab.set_peak(meter.level.peak_db);
                tab.set_rms(meter.level.rms_db);
                tab.set_gate_open(meter.gate_open);
            }
            None => {
                tab.set_metering(false);
                tab.set_peak(SILENCE_DB);
                tab.set_rms(SILENCE_DB);
                tab.set_gate_open(false);
            }
        }
        keep = status.is_running();
    });
    if !keep && let Some(d) = app.discord.as_ref() {
        d.poll.stop();
    }
    app.update_tray_discord();
}

fn show_status(d: &DiscordUi, tab: &DiscordTab<'_>, status: &BotStatus) {
    tab.set_running(status.is_running());
    tab.set_failed(matches!(status, BotStatus::Failed { .. }));
    let has_token = d.settings.borrow().token().is_some();
    let (text, hint) = match status {
        BotStatus::Off if has_token => ("Not logged in.".to_owned(), String::new()),
        BotStatus::Off => (
            "Paste the bot's token and log in.".to_owned(),
            "A Discord bot account is needed: \"How to set up a bot\" shows the steps.".to_owned(),
        ),
        BotStatus::LoggingIn => ("Logging in...".to_owned(), String::new()),
        BotStatus::Online(online) => (
            format!("Logged in as {}.", online.user),
            if online.servers.is_empty() {
                "The bot isn't in any server yet: use \"Invite to a server\".".to_owned()
            } else {
                String::new()
            },
        ),
        BotStatus::Failed { reason, hint } => (capitalize(reason), hint.clone()),
    };
    tab.set_status_text(text.into());
    tab.set_hint(hint.into());

    let online = status.online();
    tab.set_online(online.is_some());
    tab.set_can_invite(online.is_some_and(|o| o.invite_url.is_some()));
    tab.set_in_voice(status.voice().is_some());
    show_servers(d, tab, online);
    tab.set_voice_text(online.map(voice_text).unwrap_or_default().into());
    let notice = online.and_then(|o| o.notice.as_ref());
    tab.set_notice(
        notice
            .map(|n| capitalize(&n.text))
            .unwrap_or_default()
            .into(),
    );
    tab.set_notice_hint(notice.map(|n| n.hint.clone()).unwrap_or_default().into());
    let (input_text, input_failed) = match online.map(|o| &o.input) {
        Some(InputStatus::Open { device }) => (format!("Recording from {device}."), false),
        Some(InputStatus::Failed { reason }) => (capitalize(reason), true),
        _ => (String::new(), false),
    };
    tab.set_input_text(input_text.into());
    tab.set_input_failed(input_failed);
}

fn capitalize(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

fn voice_text(online: &Online) -> String {
    let Some(voice) = &online.voice else {
        return "Not in a voice channel.".into();
    };
    let server = online.server(voice.guild_id);
    let where_ = format!(
        "{} on {}",
        server
            .and_then(|s| s.channel(voice.channel_id))
            .map_or("the channel", |c| c.name.as_str()),
        server.map_or("the server", |s| s.name.as_str()),
    );
    match voice.state {
        VoiceState::Joining => format!("Joining {where_}..."),
        VoiceState::Retrying { attempt, in_secs } => format!(
            "The connection to {where_} dropped; trying again in {in_secs} s (attempt {attempt})."
        ),
        VoiceState::Connected => {
            let mut text = format!("In {where_}.");
            if voice.lag_ms >= 100 {
                text += &format!(" {} ms behind.", voice.lag_ms);
            }
            if voice.dropped > 0 {
                text += &format!(" {} ms of sound skipped to stay live.", voice.dropped * 20);
            }
            text
        }
    }
}

/// The server and channel pickers. Rebuilt only when the servers change, so a choice being
/// made isn't reset; the bot's current channel is picked when it joins or is moved.
fn show_servers(d: &DiscordUi, tab: &DiscordTab<'_>, online: Option<&Online>) {
    let servers = online.map(|o| o.servers.clone()).unwrap_or_default();
    if let Some(voice) = online.and_then(|o| o.voice.as_ref()) {
        let now = Some((voice.guild_id, Some(voice.channel_id)));
        if d.picked.get() != now {
            d.picked.set(now);
            *d.servers.borrow_mut() = Vec::new();
        }
    }
    if *d.servers.borrow() == servers {
        return;
    }
    tab.set_servers(strings(servers.iter().map(|s| s.name.clone())));
    let index = d
        .picked
        .get()
        .and_then(|(g, _)| servers.iter().position(|s| s.id == g))
        .unwrap_or(0);
    *d.servers.borrow_mut() = servers;
    tab.set_server(if d.servers.borrow().is_empty() {
        -1
    } else {
        index as i32
    });
    show_channels(d, tab);
}

fn show_channels(d: &DiscordUi, tab: &DiscordTab<'_>) {
    let servers = d.servers.borrow();
    let server = usize::try_from(tab.get_server())
        .ok()
        .and_then(|i| servers.get(i));
    let channels = server.map(|s| s.channels.as_slice()).unwrap_or_default();
    tab.set_channels(strings(channels.iter().map(|c| {
        let mut label = c.name.clone();
        if c.stage {
            label += " (stage)";
        }
        if !c.can_speak {
            label += " (no permission to speak)";
        }
        label
    })));
    let wanted = d
        .picked
        .get()
        .filter(|(g, _)| server.is_some_and(|s| s.id == *g))
        .and_then(|(_, c)| c);
    let index = wanted
        .and_then(|c| channels.iter().position(|ch| ch.id == c))
        .unwrap_or(0);
    tab.set_channel(if channels.is_empty() {
        -1
    } else {
        index as i32
    });
}

fn picked_ids(d: &DiscordUi, tab: &DiscordTab<'_>) -> Option<(u64, u64)> {
    let servers = d.servers.borrow();
    let server = servers.get(usize::try_from(tab.get_server()).ok()?)?;
    let channel = server
        .channels
        .get(usize::try_from(tab.get_channel()).ok()?)?;
    Some((server.id, channel.id))
}

// ----- callbacks -----

fn login(app: &Rc<App>) {
    let mut started = false;
    with_tab(app, |d, tab| {
        let typed = tab.get_token().trim().to_owned();
        if !typed.is_empty() {
            d.settings.borrow_mut().token = Some(Secret::new(typed));
            tab.set_token(SharedString::new());
            tab.set_show_token(false);
            d.save(app);
            tab.set_has_token(true);
        }
        started = discord::login(&d.bot, &d.settings.borrow());
    });
    if started {
        start_polling(app);
        refresh_devices(app);
    }
}

fn logout(app: &Rc<App>) {
    with_tab(app, |d, _| d.bot.logout());
    start_polling(app);
}

fn invite(app: &Rc<App>) {
    let url = app.discord.as_ref().and_then(|d| {
        d.status
            .borrow()
            .borrow()
            .online()
            .and_then(|o| o.invite_url.clone())
    });
    if let Some(url) = url {
        open_url(app, &url);
    }
}

fn open_url(app: &Rc<App>, url: &str) {
    if let Err(e) = rtspcam_platform::desktop::open_url(url) {
        message::inform(
            app,
            "Discord",
            &format!("Could not open the browser: {e}\n\n{url}"),
        );
    }
}

fn join(app: &Rc<App>) {
    with_tab(app, |d, tab| {
        let Some((guild, channel)) = picked_ids(d, tab) else {
            return;
        };
        d.picked.set(Some((guild, Some(channel))));
        d.bot.join(guild, channel);
        d.settings.borrow_mut().last_channel = Some(ChannelRef::new(guild, channel));
        d.save(app);
    });
}

fn leave(app: &Rc<App>) {
    with_tab(app, |d, _| {
        d.bot.leave();
        // Left on purpose: don't join again at the next login.
        d.settings.borrow_mut().last_channel = None;
        d.save(app);
    });
}

fn choose_device(app: &Rc<App>, index: i32) {
    with_tab(app, |d, _| {
        let Some(id) = usize::try_from(index)
            .ok()
            .and_then(|i| d.devices.borrow().get(i).map(|dev| dev.id.clone()))
        else {
            return;
        };
        if d.settings.borrow().input_device == id {
            return;
        }
        d.bot.set_input(&id);
        d.settings.borrow_mut().input_device = id;
        d.save(app);
    });
}

fn gate_changed(app: &Rc<App>) {
    with_tab(app, |d, tab| {
        {
            let mut s = d.settings.borrow_mut();
            s.gate.enabled = tab.get_gate_enabled();
            s.gate.threshold_db = tab.get_threshold().round();
        }
        d.bot.set_gate(discord::gate(&d.settings.borrow()));
        let me = Rc::downgrade(app);
        d.save_later
            .start(TimerMode::SingleShot, SAVE_DELAY, move || {
                if let Some(app) = me.upgrade()
                    && let Some(d) = app.discord.as_ref()
                {
                    d.save(&app);
                }
            });
    });
}

/// Mute or unmute (the tab's button and the tray's menu).
pub(crate) fn toggle_mute(app: &Rc<App>) {
    with_tab(app, |d, tab| {
        let muted = !d.settings.borrow().muted;
        d.settings.borrow_mut().muted = muted;
        d.bot.set_muted(muted);
        tab.set_muted(muted);
        d.save(app);
    });
    app.update_tray_discord();
}

/// Applies hand edits of `discord.json`. Called from the window's regular tick.
pub(crate) fn tick(app: &Rc<App>) {
    with_tab(app, |d, tab| {
        let Some(new) = d
            .external
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
        else {
            return;
        };
        tracing::info!("discord.json changed on disk");
        discord::apply_change(&d.bot, &d.settings.borrow(), &new);
        *d.settings.borrow_mut() = new;
        show_settings(d, tab);
        show_devices(d, tab);
    });
    if app
        .discord
        .as_ref()
        .is_some_and(|d| d.bot.status().borrow().is_running())
    {
        start_polling(app);
    }
}

/// Wires the tab's callbacks. Called once.
pub(crate) fn connect(app: &Rc<App>) {
    let Some(ui) = app.ui() else { return };
    let tab = ui.global::<DiscordTab>();
    macro_rules! on {
        ($callback:ident, $f:expr) => {{
            let me = Rc::downgrade(app);
            tab.$callback(move || {
                if let Some(app) = me.upgrade() {
                    $f(&app);
                }
            });
        }};
    }
    on!(on_login, login);
    on!(on_logout, logout);
    on!(on_invite, invite);
    on!(on_setup_help, |app: &Rc<App>| open_url(app, SETUP_HELP_URL));
    on!(on_join, join);
    on!(on_leave, leave);
    on!(on_refresh_devices, refresh_devices);
    on!(on_gate_changed, gate_changed);
    on!(on_toggle_mute, toggle_mute);

    let me = Rc::downgrade(app);
    tab.on_auto_login_changed(move |on| {
        if let Some(app) = me.upgrade() {
            with_tab(&app, |d, _| {
                d.settings.borrow_mut().auto_login = on;
                d.save(&app);
            });
        }
    });
    let me = Rc::downgrade(app);
    tab.on_server_chosen(move |index| {
        if let Some(app) = me.upgrade() {
            with_tab(&app, |d, tab| {
                let id = usize::try_from(index)
                    .ok()
                    .and_then(|i| d.servers.borrow().get(i).map(|s| s.id));
                d.picked.set(id.map(|g| (g, None)));
                show_channels(d, tab);
            });
        }
    });
    let me = Rc::downgrade(app);
    tab.on_device_chosen(move |index| {
        if let Some(app) = me.upgrade() {
            choose_device(&app, index);
        }
    });
}
