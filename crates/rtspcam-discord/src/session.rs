//! One logged-in session: the state machine behind [`BotStatus`], independent of serenity and
//! songbird (they sit behind [`Backend`], so the logic is tested without a network).

use std::collections::BTreeMap;
use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use rtspcam_audio::{AudioError, BlockQueue, GateSettings};
use tokio::sync::{mpsc, watch};
use tokio::time::Instant;

use crate::audio::{AudioShared, Devices, ListenShared};
use crate::error::BotError;
use crate::status::{
    BotStatus, DeviceStatus, Notice, Online, Server, Stage, VoiceState, VoiceStatus,
};

/// Waits between attempts to rejoin a channel after the connection dropped (the last one
/// repeats), the same shape as the camera pipeline's backoff.
const REJOIN_BACKOFF: [u64; 5] = [1, 2, 5, 10, 30];
/// A sound device that failed is tried again this often while in a channel.
const DEVICE_RETRY: Duration = Duration::from_secs(3);
/// How often the lag shown in the status is brought up to date.
const STATUS_TICK: Duration = Duration::from_millis(500);

/// What a session starts with.
#[derive(Debug, Clone, PartialEq)]
pub struct SessionOptions {
    /// The sound source's device id (see `rtspcam_audio::DeviceInfo::id`).
    pub input: String,
    pub muted: bool,
    pub gate: GateSettings,
    /// Hearing the channel.
    pub listen: ListenSettings,
    /// Per server (by id): what to join on its own.
    pub servers: BTreeMap<u64, ServerRules>,
    /// A channel to join as soon as the bot is ready, if no server rule picks one: (server id,
    /// channel id).
    pub rejoin: Option<(u64, u64)>,
}

impl Default for SessionOptions {
    fn default() -> Self {
        Self {
            input: rtspcam_audio::DEFAULT_DEVICE.to_owned(),
            muted: true,
            gate: GateSettings::default(),
            listen: ListenSettings::default(),
            servers: BTreeMap::new(),
            rejoin: None,
        }
    }
}

/// Playing the channel on an output device (documentation/19 §4 D5).
#[derive(Debug, Clone, PartialEq)]
pub struct ListenSettings {
    pub enabled: bool,
    /// The output device's id (`default` or an `output:` id).
    pub output: String,
    /// 1.0 plays the channel as received.
    pub volume: f32,
}

impl Default for ListenSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            output: rtspcam_audio::DEFAULT_DEVICE.to_owned(),
            volume: 1.0,
        }
    }
}

/// What the bot does on its own in one server (documentation/19 §4 D6).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ServerRules {
    /// A channel to join after logging in.
    pub auto_join: Option<u64>,
    /// A user to follow: when they join, move or leave a voice channel in this server, the
    /// bot does the same. Wins over `auto_join` when they are in voice at login.
    pub follow: Option<u64>,
}

/// What the UI asks for.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Command {
    Join {
        guild: u64,
        channel: u64,
    },
    Leave,
    SetInput(String),
    SetMuted(bool),
    SetGate(GateSettings),
    SetListen(ListenSettings),
    SetServerRules(BTreeMap<u64, ServerRules>),
    /// Takes the bot out of a server for good (it needs a new invite to come back).
    LeaveServer(u64),
    Logout,
}

/// What the backend reports.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Event {
    /// The gateway session is ready and the servers are known.
    Ready,
    /// Servers or channels changed (the backend's cache has the new ones).
    ServersChanged,
    /// The voice connection is up (again).
    VoiceConnected {
        guild: u64,
    },
    /// The bot's own voice state changed: moved to `channel`, or out of voice (`None`).
    /// `suppressed`: in a stage channel's audience.
    VoiceMoved {
        guild: u64,
        channel: Option<u64>,
        suppressed: bool,
    },
    /// Someone else joined, moved or left (`None`) a voice channel.
    UserVoice {
        guild: u64,
        user: u64,
        channel: Option<u64>,
    },
    /// The voice connection ended without being asked to.
    VoiceLost {
        guild: u64,
        error: BotError,
    },
    /// The gateway connection ended for good.
    GatewayLost(BotError),
    InputFailed(AudioError),
    OutputFailed(AudioError),
}

/// Logged in, as the backend reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LoginInfo {
    pub(crate) user: String,
    pub(crate) app_id: Option<u64>,
}

/// The Discord side: gateway and voice. The real one is `SerenityBackend`.
pub(crate) trait Backend: Send + 'static {
    /// Checks the token, starts the gateway, and sends [`Event`]s to `events` from then on.
    fn login(
        &mut self,
        token: &str,
        events: mpsc::UnboundedSender<Event>,
    ) -> impl Future<Output = Result<LoginInfo, BotError>> + Send;
    /// The servers, their voice channels and who is in them, as known now.
    fn servers(&self) -> Vec<Server>;
    /// Joins and starts sending the sound in `queue`; hands what is heard to `listen`.
    fn join(
        &mut self,
        guild: u64,
        channel: u64,
        queue: Arc<BlockQueue>,
        muted: bool,
        listen: Arc<ListenShared>,
    ) -> impl Future<Output = Result<(), BotError>> + Send;
    fn leave(&mut self, guild: u64) -> impl Future<Output = ()> + Send;
    fn set_muted(&mut self, guild: u64, muted: bool) -> impl Future<Output = ()> + Send;
    /// Turns decoding what is heard on or off (it costs CPU, so only while listening).
    fn set_listening(&mut self, guild: u64, on: bool) -> impl Future<Output = ()> + Send;
    /// In a stage channel: becomes a speaker if the bot may, otherwise asks to speak.
    fn speak_on_stage(
        &mut self,
        guild: u64,
        channel: u64,
    ) -> impl Future<Output = Result<Stage, BotError>> + Send;
    fn leave_server(&mut self, guild: u64) -> impl Future<Output = Result<(), BotError>> + Send;
    /// Leaves every channel and closes the gateway.
    fn logout(&mut self) -> impl Future<Output = ()> + Send;
}

/// Publishes the status, unless a newer session has started since (a slow logout must not
/// overwrite the next login's status).
#[derive(Debug, Clone)]
pub(crate) struct StatusOut {
    pub(crate) tx: Arc<watch::Sender<BotStatus>>,
    pub(crate) generation: Arc<AtomicU64>,
    pub(crate) mine: u64,
}

impl StatusOut {
    fn current(&self) -> bool {
        self.generation.load(Ordering::SeqCst) == self.mine
    }

    pub(crate) fn set(&self, status: BotStatus) {
        if self.current() {
            self.tx.send_if_modified(|s| {
                if *s == status {
                    false
                } else {
                    *s = status;
                    true
                }
            });
        }
    }
}

pub(crate) struct Session<B, D> {
    backend: B,
    devices: Arc<D>,
    out: StatusOut,
    shared: Arc<AudioShared>,
    listen: Arc<ListenShared>,
    options: SessionOptions,
    online: Online,
    events_tx: mpsc::UnboundedSender<Event>,
    events: mpsc::UnboundedReceiver<Event>,
    /// The channel the user wants the bot in.
    wanted: Option<(u64, u64)>,
    capture: Option<Box<dyn Send>>,
    playback: Option<Box<dyn Send>>,
    /// When to try joining again, and the attempt number.
    rejoin_at: Option<(Instant, u32)>,
    /// When to try opening a failed sound device again.
    device_retry_at: Option<Instant>,
    /// The first ready event joins a channel once (server rules, then `options.rejoin`).
    auto_joined: bool,
}

impl<B: Backend, D: Devices> Session<B, D> {
    pub(crate) fn new(
        backend: B,
        devices: Arc<D>,
        out: StatusOut,
        shared: Arc<AudioShared>,
        options: SessionOptions,
    ) -> Self {
        let (events_tx, events) = mpsc::unbounded_channel();
        let listen = Arc::new(ListenShared::new(
            options.listen.enabled,
            options.listen.volume,
        ));
        Self {
            backend,
            devices,
            out,
            shared,
            listen,
            online: Online {
                muted: options.muted,
                listening: options.listen.enabled,
                ..Online::default()
            },
            options,
            events_tx,
            events,
            wanted: None,
            capture: None,
            playback: None,
            rejoin_at: None,
            device_retry_at: None,
            auto_joined: false,
        }
    }

    fn publish(&self) {
        self.out.set(BotStatus::Online(self.online.clone()));
    }

    /// Runs until logged out (or the command channel closes, or logging in fails).
    pub(crate) async fn run(
        mut self,
        token: String,
        mut commands: mpsc::UnboundedReceiver<Command>,
    ) {
        self.out.set(BotStatus::LoggingIn);
        let login = self.backend.login(&token, self.events_tx.clone()).await;
        drop(token);
        let info = match login {
            Ok(info) => info,
            Err(e) => {
                tracing::warn!(error = %e, "Discord login failed");
                self.out.set(BotStatus::Failed {
                    reason: e.to_string(),
                    hint: e.hint(),
                });
                return;
            }
        };
        tracing::info!(user = %info.user, "logged in to Discord");
        self.online.user = info.user;
        self.online.invite_url = info.app_id.map(crate::invite_url);
        self.online.servers = self.backend.servers();
        self.publish();

        let mut tick = tokio::time::interval(STATUS_TICK);
        loop {
            let rejoin_at = self.rejoin_at.map(|(at, _)| at);
            let device_retry_at = self.device_retry_at;
            tokio::select! {
                command = commands.recv() => match command {
                    Some(Command::Logout) | None => break,
                    Some(command) => self.handle_command(command).await,
                },
                Some(event) = self.events.recv() => {
                    if let Event::GatewayLost(e) = &event {
                        tracing::warn!(error = %e, "the Discord connection ended");
                        self.close_devices();
                        self.backend.logout().await;
                        self.out.set(BotStatus::Failed { reason: e.to_string(), hint: e.hint() });
                        return;
                    }
                    self.handle_event(event).await;
                }
                () = sleep_until(rejoin_at) => self.retry_join().await,
                () = sleep_until(device_retry_at) => {
                    self.device_retry_at = None;
                    self.open_devices().await;
                    self.publish();
                }
                _ = tick.tick() => self.update_voice_numbers(),
            }
        }

        tracing::info!("logging out of Discord");
        self.close_devices();
        self.backend.logout().await;
        self.out.set(BotStatus::Off);
    }

    async fn handle_command(&mut self, command: Command) {
        match command {
            Command::Join { guild, channel } => self.join(guild, channel).await,
            Command::Leave => self.leave().await,
            Command::SetInput(id) => {
                self.options.input = id;
                if self.capture.is_some() || self.wanted.is_some() {
                    // Swap the source without leaving the channel.
                    self.close_capture();
                    self.open_devices().await;
                }
                self.publish();
            }
            Command::SetMuted(muted) => {
                self.shared.set_muted(muted);
                self.online.muted = muted;
                if let Some((guild, _)) = self.wanted {
                    self.backend.set_muted(guild, muted).await;
                }
                self.publish();
            }
            Command::SetGate(gate) => {
                self.options.gate = gate;
                self.shared.set_gate(gate);
            }
            Command::SetListen(listen) => self.set_listen(listen).await,
            Command::SetServerRules(rules) => self.set_server_rules(rules).await,
            Command::LeaveServer(guild) => self.leave_server(guild).await,
            Command::Logout => {}
        }
    }

    async fn handle_event(&mut self, event: Event) {
        match event {
            Event::Ready => {
                self.online.servers = self.backend.servers();
                self.publish();
                if !self.auto_joined {
                    self.auto_joined = true;
                    if self.wanted.is_none()
                        && let Some((guild, channel)) = self.startup_channel()
                    {
                        tracing::info!(guild, channel, "joining a channel after logging in");
                        self.join(guild, channel).await;
                    }
                }
            }
            Event::ServersChanged => {
                self.online.servers = self.backend.servers();
                self.publish();
            }
            Event::VoiceConnected { guild } => {
                if let Some(voice) = self.voice_in(guild) {
                    voice.state = VoiceState::Connected;
                    self.publish();
                }
            }
            Event::VoiceMoved {
                guild,
                channel: Some(channel),
                suppressed,
            } => self.moved(guild, channel, suppressed).await,
            Event::VoiceMoved {
                guild,
                channel: None,
                ..
            } => {
                let connected = self
                    .voice_in(guild)
                    .is_some_and(|v| v.state == VoiceState::Connected);
                if connected {
                    self.give_up(guild, BotError::Kicked).await;
                }
            }
            Event::UserVoice {
                guild,
                user,
                channel,
            } => {
                self.online.servers = self.backend.servers();
                if self.followed(guild) == Some(user) {
                    self.follow(guild, user, channel).await;
                }
                self.publish();
            }
            Event::VoiceLost { guild, error } => {
                if self.wanted.is_some_and(|(g, _)| g == guild) {
                    tracing::warn!(guild, error = %error, "voice connection lost");
                    if error.worth_retrying() {
                        self.schedule_rejoin(1);
                    } else {
                        self.give_up(guild, error).await;
                    }
                }
            }
            Event::InputFailed(e) => {
                self.close_capture();
                self.device_failed(e, true);
                self.publish();
            }
            Event::OutputFailed(e) => {
                self.close_playback();
                self.device_failed(e, false);
                self.publish();
            }
            // Handled by `run`.
            Event::GatewayLost(_) => {}
        }
    }

    fn voice_in(&mut self, guild: u64) -> Option<&mut VoiceStatus> {
        self.online.voice.as_mut().filter(|v| v.guild_id == guild)
    }

    fn followed(&self, guild: u64) -> Option<u64> {
        self.options.servers.get(&guild).and_then(|r| r.follow)
    }

    /// The channel to join after logging in: where a followed user is, else a server's
    /// auto-join channel (in the order the servers are listed), else the last channel.
    fn startup_channel(&self) -> Option<(u64, u64)> {
        let servers = &self.online.servers;
        let rules = |s: &Server| self.options.servers.get(&s.id).copied().unwrap_or_default();
        let following = servers.iter().find_map(|s| {
            let member = s.member(rules(s).follow?)?;
            Some((s.id, member.channel_id))
        });
        let auto_join = || {
            servers.iter().find_map(|s| {
                let channel = rules(s).auto_join?;
                s.channel(channel).map(|c| (s.id, c.id))
            })
        };
        following.or_else(auto_join).or(self.options.rejoin)
    }

    /// The followed `user` joined, moved to, or left (`None`) a channel in `guild`.
    async fn follow(&mut self, guild: u64, user: u64, channel: Option<u64>) {
        match channel {
            Some(channel) if self.wanted != Some((guild, channel)) => {
                tracing::info!(guild, user, channel, "following a user into a channel");
                self.join(guild, channel).await;
            }
            None if self.wanted.is_some_and(|(g, _)| g == guild) => {
                tracing::info!(guild, user, "the followed user left voice; leaving too");
                self.leave().await;
            }
            _ => {}
        }
    }

    async fn set_server_rules(&mut self, rules: BTreeMap<u64, ServerRules>) {
        let old = std::mem::replace(&mut self.options.servers, rules);
        // Someone newly followed who is in voice now: go to them.
        let newly_followed: Vec<(u64, u64)> = self
            .options
            .servers
            .iter()
            .filter_map(|(guild, r)| {
                let user = r.follow?;
                (old.get(guild).and_then(|o| o.follow) != Some(user)).then_some((*guild, user))
            })
            .collect();
        for (guild, user) in newly_followed {
            let channel = self
                .online
                .server(guild)
                .and_then(|s| s.member(user))
                .map(|m| m.channel_id);
            if channel.is_some() {
                self.follow(guild, user, channel).await;
            }
        }
    }

    async fn moved(&mut self, guild: u64, channel: u64, suppressed: bool) {
        let Some((wanted_guild, wanted_channel)) = self.wanted else {
            return;
        };
        if wanted_guild != guild {
            return;
        }
        if wanted_channel != channel {
            tracing::info!(guild, channel, "the bot was moved to another channel");
            self.wanted = Some((guild, channel));
            if let Some(voice) = self.voice_in(guild) {
                voice.channel_id = channel;
                voice.stage = None;
            }
            if self
                .voice_in(guild)
                .is_some_and(|v| v.state == VoiceState::Connected)
            {
                self.go_on_stage(guild, channel).await;
            }
        }
        if let Some(voice) = self.voice_in(guild)
            && let Some(stage) = &mut voice.stage
        {
            *stage = match (*stage, suppressed) {
                (_, false) => Stage::Speaker,
                (Stage::Speaker, true) => Stage::Audience,
                (other, true) => other,
            };
        }
        self.publish();
    }

    fn is_stage(&self, guild: u64, channel: u64) -> bool {
        self.online
            .server(guild)
            .and_then(|s| s.channel(channel))
            .is_some_and(|c| c.stage)
    }

    /// In a stage channel, asks to speak (or becomes a speaker straight away if allowed).
    async fn go_on_stage(&mut self, guild: u64, channel: u64) {
        if !self.is_stage(guild, channel) {
            return;
        }
        let stage = match self.backend.speak_on_stage(guild, channel).await {
            Ok(stage) => stage,
            Err(e) => {
                tracing::warn!(guild, channel, error = %e, "could not ask to speak on the stage");
                self.online.notice = Some(Notice {
                    text: format!("could not ask to speak on the stage: {e}"),
                    hint: "A stage moderator can invite the bot to speak, or give its role \
                           the Request to Speak permission."
                        .into(),
                });
                Stage::Audience
            }
        };
        if let Some(voice) = self.voice_in(guild) {
            // A voice state update may already have made it a speaker.
            if voice.stage != Some(Stage::Speaker) {
                voice.stage = Some(stage);
            }
        }
    }

    async fn join(&mut self, guild: u64, channel: u64) {
        // Already somewhere else on another server: leave that first (one channel at a time).
        if let Some((old_guild, _)) = self.wanted
            && old_guild != guild
        {
            self.backend.leave(old_guild).await;
        }
        self.wanted = Some((guild, channel));
        self.rejoin_at = None;
        self.online.voice = Some(VoiceStatus {
            guild_id: guild,
            channel_id: channel,
            state: VoiceState::Joining,
            lag_ms: 0,
            dropped: self.shared.queue.dropped(),
            stage: None,
            speakers: 0,
        });
        self.publish();
        self.open_devices().await;
        self.connect(guild, channel, 0).await;
    }

    /// One attempt at the voice connection; on failure either schedules the next one or gives
    /// up, depending on the error.
    async fn connect(&mut self, guild: u64, channel: u64, attempt: u32) {
        match self
            .backend
            .join(
                guild,
                channel,
                self.shared.queue.clone(),
                self.online.muted,
                self.listen.clone(),
            )
            .await
        {
            Ok(()) => {
                tracing::info!(guild, channel, "joined the voice channel");
                self.rejoin_at = None;
                self.online.notice = None;
                if let Some(voice) = self.voice_in(guild) {
                    voice.state = VoiceState::Connected;
                }
                self.go_on_stage(guild, channel).await;
                self.publish();
            }
            Err(e) if e.worth_retrying() => {
                tracing::warn!(guild, channel, error = %e, attempt, "joining failed; will retry");
                self.online.notice = Some(Notice {
                    text: e.to_string(),
                    hint: e.hint(),
                });
                self.schedule_rejoin(attempt + 1);
            }
            Err(e) => {
                tracing::warn!(guild, channel, error = %e, "joining failed");
                self.give_up(guild, e).await;
            }
        }
    }

    fn schedule_rejoin(&mut self, attempt: u32) {
        let index = (attempt.max(1) as usize - 1).min(REJOIN_BACKOFF.len() - 1);
        let wait = REJOIN_BACKOFF[index];
        self.rejoin_at = Some((Instant::now() + Duration::from_secs(wait), attempt));
        if let Some((guild, _)) = self.wanted
            && let Some(voice) = self.voice_in(guild)
        {
            voice.state = VoiceState::Retrying {
                attempt,
                in_secs: wait,
            };
        }
        self.publish();
    }

    async fn retry_join(&mut self) {
        let Some((_, attempt)) = self.rejoin_at.take() else {
            return;
        };
        let Some((guild, channel)) = self.wanted else {
            return;
        };
        if let Some(voice) = self.voice_in(guild) {
            voice.state = VoiceState::Joining;
        }
        self.publish();
        self.connect(guild, channel, attempt).await;
    }

    /// Out of the channel for good, with the reason shown.
    async fn give_up(&mut self, guild: u64, error: BotError) {
        self.online.notice = Some(Notice {
            text: error.to_string(),
            hint: error.hint(),
        });
        self.backend.leave(guild).await;
        self.forget_channel();
        self.publish();
    }

    async fn leave(&mut self) {
        if let Some((guild, _)) = self.wanted {
            tracing::info!(guild, "leaving the voice channel");
            self.backend.leave(guild).await;
        }
        self.forget_channel();
        self.publish();
    }

    async fn leave_server(&mut self, guild: u64) {
        if self.wanted.is_some_and(|(g, _)| g == guild) {
            self.leave().await;
        }
        tracing::info!(guild, "leaving the server");
        match self.backend.leave_server(guild).await {
            Ok(()) => {
                self.online.servers.retain(|s| s.id != guild);
                self.options.servers.remove(&guild);
            }
            Err(e) => {
                tracing::warn!(guild, error = %e, "could not leave the server");
                self.online.notice = Some(Notice {
                    text: format!("could not leave the server: {e}"),
                    hint: e.hint(),
                });
            }
        }
        self.publish();
    }

    fn forget_channel(&mut self) {
        self.wanted = None;
        self.rejoin_at = None;
        self.device_retry_at = None;
        self.online.voice = None;
        self.close_devices();
    }

    async fn set_listen(&mut self, listen: ListenSettings) {
        let old = std::mem::replace(&mut self.options.listen, listen.clone());
        self.listen.set(listen.enabled, listen.volume);
        self.online.listening = listen.enabled;
        if let Some((guild, _)) = self.wanted
            && old.enabled != listen.enabled
        {
            self.backend.set_listening(guild, listen.enabled).await;
        }
        if !listen.enabled || old.output != listen.output {
            self.close_playback();
        }
        self.open_devices().await;
        self.publish();
    }

    /// Opens whatever the channel needs and isn't open yet: the source, and the output while
    /// listening. Only in a channel.
    async fn open_devices(&mut self) {
        if self.wanted.is_none() {
            return;
        }
        if self.capture.is_none() {
            let devices = self.devices.clone();
            let id = self.options.input.clone();
            let sink = Box::new(self.shared.sink());
            let events = self.events_tx.clone();
            let on_error = Box::new(move |e| {
                let _ = events.send(Event::InputFailed(e));
            });
            match blocking(move || devices.open_input(&id, sink, on_error)).await {
                Ok((capture, device)) => {
                    self.capture = Some(capture);
                    self.online.input = DeviceStatus::Open { device };
                    self.clear_device_notice();
                }
                Err(e) => {
                    tracing::warn!(device = %self.options.input, error = %e, "could not open the sound source");
                    self.device_failed(e, true);
                }
            }
        }
        if self.options.listen.enabled && self.playback.is_none() {
            let devices = self.devices.clone();
            let id = self.options.listen.output.clone();
            let queue = self.listen.queue.clone();
            let events = self.events_tx.clone();
            let on_error = Box::new(move |e| {
                let _ = events.send(Event::OutputFailed(e));
            });
            queue.clear();
            match blocking(move || devices.open_output(&id, queue, on_error)).await {
                Ok((playback, device)) => {
                    self.playback = Some(playback);
                    self.online.output = DeviceStatus::Open { device };
                    self.clear_device_notice();
                }
                Err(e) => {
                    tracing::warn!(device = %self.options.listen.output, error = %e, "could not open the output");
                    self.device_failed(e, false);
                }
            }
        }
    }

    /// A device didn't open or stopped: shown, and tried again while in a channel.
    fn device_failed(&mut self, e: AudioError, input: bool) {
        let failed = DeviceStatus::Failed {
            reason: e.to_string(),
        };
        let e = if input {
            self.online.input = failed;
            BotError::Input(e)
        } else {
            self.online.output = failed;
            BotError::Output(e)
        };
        self.online.notice = Some(Notice {
            text: e.to_string(),
            hint: e.hint(),
        });
        if self.wanted.is_some() {
            self.device_retry_at = Some(Instant::now() + DEVICE_RETRY);
        }
    }

    fn clear_device_notice(&mut self) {
        let input_ok = !matches!(self.online.input, DeviceStatus::Failed { .. });
        let output_ok = !matches!(self.online.output, DeviceStatus::Failed { .. });
        if input_ok && output_ok {
            self.device_retry_at = None;
            if matches!(
                &self.online.notice,
                Some(n) if n.text.starts_with("the sound source") || n.text.starts_with("the output")
            ) {
                self.online.notice = None;
            }
        }
    }

    fn close_capture(&mut self) {
        if self.capture.take().is_some() {
            tracing::info!("sound source closed");
        }
        self.shared.reset_meter();
        if !matches!(self.online.input, DeviceStatus::Failed { .. }) || self.wanted.is_none() {
            self.online.input = DeviceStatus::Closed;
        }
    }

    fn close_playback(&mut self) {
        if self.playback.take().is_some() {
            tracing::info!("channel playback closed");
        }
        self.listen.queue.clear();
        if !matches!(self.online.output, DeviceStatus::Failed { .. })
            || self.wanted.is_none()
            || !self.options.listen.enabled
        {
            self.online.output = DeviceStatus::Closed;
        }
    }

    fn close_devices(&mut self) {
        self.close_capture();
        self.close_playback();
    }

    fn update_voice_numbers(&mut self) {
        let lag_ms = self.shared.queue.lag().as_millis() as u32;
        let dropped = self.shared.queue.dropped();
        let speakers = if self.listen.enabled() {
            self.listen.speakers()
        } else {
            0
        };
        let Some(voice) = self.online.voice.as_mut() else {
            return;
        };
        if voice.lag_ms != lag_ms || voice.dropped != dropped || voice.speakers != speakers {
            voice.lag_ms = lag_ms;
            voice.dropped = dropped;
            voice.speakers = speakers;
            self.publish();
        }
    }
}

/// Runs slow device work on a blocking thread.
async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, AudioError> + Send + 'static,
) -> Result<T, AudioError> {
    tokio::task::spawn_blocking(f)
        .await
        .unwrap_or_else(|e| Err(AudioError::Other(e.to_string())))
}

async fn sleep_until(at: Option<Instant>) {
    match at {
        Some(at) => tokio::time::sleep_until(at).await,
        None => std::future::pending().await,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::Mutex;

    use rtspcam_audio::Block;

    use super::*;
    use crate::status::{VoiceChannel, VoiceMember};

    /// Records what it was asked to do; answers joins from a script (default: success).
    #[derive(Clone, Default)]
    struct FakeBackend {
        calls: Arc<Mutex<Vec<String>>>,
        login: Arc<Mutex<Option<BotError>>>,
        joins: Arc<Mutex<VecDeque<Result<(), BotError>>>>,
        events: Arc<Mutex<Option<mpsc::UnboundedSender<Event>>>>,
        /// Who is in voice: (server, user, channel).
        members: Arc<Mutex<Vec<(u64, u64, u64)>>>,
        /// What asking to speak on a stage gives (default: a speaker).
        stage: Arc<Mutex<Option<Result<Stage, BotError>>>>,
    }

    impl FakeBackend {
        fn calls(&self) -> Vec<String> {
            self.calls.lock().unwrap().clone()
        }

        fn send(&self, event: Event) {
            self.events
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .send(event)
                .unwrap();
        }

        fn log(&self, call: String) {
            self.calls.lock().unwrap().push(call);
        }
    }

    impl Backend for FakeBackend {
        async fn login(
            &mut self,
            token: &str,
            events: mpsc::UnboundedSender<Event>,
        ) -> Result<LoginInfo, BotError> {
            self.log(format!("login {token}"));
            *self.events.lock().unwrap() = Some(events);
            match self.login.lock().unwrap().take() {
                Some(e) => Err(e),
                None => Ok(LoginInfo {
                    user: "Test Bot".into(),
                    app_id: Some(42),
                }),
            }
        }

        /// Two servers: "Home" (1) with General (10), Other (11) and a stage (12); "Away"
        /// (2) with Lounge (20).
        fn servers(&self) -> Vec<Server> {
            let channel = |id, name: &str, stage| VoiceChannel {
                id,
                name: name.into(),
                stage,
                can_speak: true,
            };
            let members = |server| {
                self.members
                    .lock()
                    .unwrap()
                    .iter()
                    .filter(|(s, ..)| *s == server)
                    .map(|&(_, id, channel_id)| VoiceMember {
                        id,
                        name: format!("user {id}"),
                        channel_id,
                    })
                    .collect()
            };
            vec![
                Server {
                    id: 1,
                    name: "Home".into(),
                    channels: vec![
                        channel(10, "General", false),
                        channel(11, "Other", false),
                        channel(12, "Stage", true),
                    ],
                    members: members(1),
                },
                Server {
                    id: 2,
                    name: "Away".into(),
                    channels: vec![channel(20, "Lounge", false)],
                    members: members(2),
                },
            ]
        }

        async fn join(
            &mut self,
            guild: u64,
            channel: u64,
            _: Arc<BlockQueue>,
            muted: bool,
            listen: Arc<ListenShared>,
        ) -> Result<(), BotError> {
            self.log(format!("join {guild} {channel} muted={muted}"));
            if listen.enabled() {
                self.log("listening".into());
            }
            self.joins.lock().unwrap().pop_front().unwrap_or(Ok(()))
        }

        async fn set_listening(&mut self, guild: u64, on: bool) {
            self.log(format!("listen {guild} {on}"));
        }

        async fn speak_on_stage(&mut self, guild: u64, channel: u64) -> Result<Stage, BotError> {
            self.log(format!("stage {guild} {channel}"));
            self.stage
                .lock()
                .unwrap()
                .take()
                .unwrap_or(Ok(Stage::Speaker))
        }

        async fn leave_server(&mut self, guild: u64) -> Result<(), BotError> {
            self.log(format!("leave server {guild}"));
            Ok(())
        }

        async fn leave(&mut self, guild: u64) {
            self.log(format!("leave {guild}"));
        }

        async fn set_muted(&mut self, guild: u64, muted: bool) {
            self.log(format!("mute {guild} {muted}"));
        }

        async fn logout(&mut self) {
            self.log("logout".into());
        }
    }

    #[derive(Default)]
    struct FakeDevices {
        opened: Mutex<Vec<String>>,
        fail: Mutex<Option<AudioError>>,
        outputs: Mutex<Vec<String>>,
        fail_output: Mutex<Option<AudioError>>,
    }

    impl Devices for FakeDevices {
        fn open_output(
            &self,
            id: &str,
            _: Arc<BlockQueue>,
            _: Box<dyn FnMut(AudioError) + Send>,
        ) -> Result<(Box<dyn Send>, String), AudioError> {
            if let Some(e) = self.fail_output.lock().unwrap().take() {
                return Err(e);
            }
            self.outputs.lock().unwrap().push(id.to_owned());
            Ok((Box::new(()), format!("Output {id}")))
        }

        fn open_input(
            &self,
            id: &str,
            mut sink: Box<dyn FnMut(Block) + Send>,
            _: Box<dyn FnMut(AudioError) + Send>,
        ) -> Result<(Box<dyn Send>, String), AudioError> {
            if let Some(e) = self.fail.lock().unwrap().take() {
                return Err(e);
            }
            self.opened.lock().unwrap().push(id.to_owned());
            sink(Block::silence());
            Ok((Box::new(()), format!("Device {id}")))
        }
    }

    struct Harness {
        backend: FakeBackend,
        inputs: Arc<FakeDevices>,
        commands: mpsc::UnboundedSender<Command>,
        status: watch::Receiver<BotStatus>,
        task: tokio::task::JoinHandle<()>,
    }

    impl Harness {
        fn start(options: SessionOptions) -> Self {
            Self::start_with(FakeBackend::default(), options)
        }

        fn start_with(backend: FakeBackend, options: SessionOptions) -> Self {
            let inputs = Arc::new(FakeDevices::default());
            let (tx, status) = watch::channel(BotStatus::Off);
            let out = StatusOut {
                tx: Arc::new(tx),
                generation: Arc::new(AtomicU64::new(1)),
                mine: 1,
            };
            let shared = Arc::new(AudioShared::new(options.muted, options.gate));
            let session = Session::new(backend.clone(), inputs.clone(), out, shared, options);
            let (commands, rx) = mpsc::unbounded_channel();
            let task = tokio::spawn(session.run("token".into(), rx));
            Self {
                backend,
                inputs,
                commands,
                status,
                task,
            }
        }

        fn send(&self, command: Command) {
            self.commands.send(command).unwrap();
        }

        /// Waits until the status matches.
        async fn until(&mut self, what: &str, f: impl Fn(&BotStatus) -> bool) -> BotStatus {
            let wait = self.status.wait_for(|s| f(s));
            let found = match tokio::time::timeout(Duration::from_secs(120), wait).await {
                Ok(Ok(s)) => Some(s.clone()),
                _ => None,
            };
            found.unwrap_or_else(|| panic!("never {what}; status: {:?}", *self.status.borrow()))
        }

        async fn voice_state(&mut self, state: VoiceState) -> BotStatus {
            self.until(&format!("{state:?}"), |s| {
                s.voice().is_some_and(|v| v.state == state)
            })
            .await
        }
    }

    #[tokio::test(start_paused = true)]
    async fn logs_in_and_shows_the_servers() {
        let mut h = Harness::start(SessionOptions::default());
        let status = h.until("online", |s| s.online().is_some()).await;
        let online = status.online().unwrap();
        assert_eq!(online.user, "Test Bot");
        assert_eq!(online.servers[0].name, "Home");
        assert!(online.muted, "starts muted");
        assert_eq!(
            online.invite_url.as_deref(),
            Some("https://discord.com/oauth2/authorize?client_id=42&scope=bot&permissions=3146752")
        );
        assert!(
            h.inputs.opened.lock().unwrap().is_empty(),
            "no device until a join"
        );

        h.send(Command::Logout);
        h.until("off", |s| *s == BotStatus::Off).await;
        h.task.await.unwrap();
        assert_eq!(h.backend.calls(), ["login token", "logout"]);
    }

    #[tokio::test(start_paused = true)]
    async fn a_refused_token_fails_with_a_hint() {
        let backend = FakeBackend::default();
        *backend.login.lock().unwrap() = Some(BotError::BadToken);
        let mut h = Harness::start_with(backend, SessionOptions::default());
        let status = h
            .until("failed", |s| matches!(s, BotStatus::Failed { .. }))
            .await;
        let BotStatus::Failed { hint, .. } = status else {
            unreachable!()
        };
        assert!(hint.contains("Developer Portal"));
        h.task.await.unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn joins_opens_the_input_and_leaves() {
        let mut h = Harness::start(SessionOptions {
            input: "input:mic".into(),
            ..SessionOptions::default()
        });
        h.until("online", |s| s.online().is_some()).await;
        h.send(Command::Join {
            guild: 1,
            channel: 10,
        });
        let status = h.voice_state(VoiceState::Connected).await;
        assert_eq!(
            status.online().unwrap().input,
            DeviceStatus::Open {
                device: "Device input:mic".into()
            }
        );
        assert_eq!(*h.inputs.opened.lock().unwrap(), ["input:mic"]);

        h.send(Command::SetMuted(false));
        h.until("unmuted", |s| s.online().is_some_and(|o| !o.muted))
            .await;

        h.send(Command::Leave);
        let status = h.until("out of voice", |s| s.voice().is_none()).await;
        assert_eq!(status.online().unwrap().input, DeviceStatus::Closed);
        assert_eq!(
            h.backend.calls(),
            [
                "login token",
                "join 1 10 muted=true",
                "mute 1 false",
                "leave 1"
            ]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn rejoins_the_last_channel_when_ready() {
        let mut h = Harness::start(SessionOptions {
            rejoin: Some((1, 10)),
            ..SessionOptions::default()
        });
        h.until("online", |s| s.online().is_some()).await;
        assert!(h.status.borrow().voice().is_none(), "waits for Ready");
        h.backend.send(Event::Ready);
        h.voice_state(VoiceState::Connected).await;
        // Only once.
        h.backend.send(Event::Ready);
        h.send(Command::Logout);
        h.task.await.unwrap();
        let joins = h
            .backend
            .calls()
            .iter()
            .filter(|c| c.starts_with("join"))
            .count();
        assert_eq!(joins, 1);
    }

    #[tokio::test(start_paused = true)]
    async fn a_dropped_connection_is_retried_with_backoff() {
        let backend = FakeBackend::default();
        backend
            .joins
            .lock()
            .unwrap()
            .extend([Err(BotError::JoinTimedOut), Err(BotError::JoinTimedOut)]);
        let mut h = Harness::start_with(backend, SessionOptions::default());
        h.until("online", |s| s.online().is_some()).await;
        h.send(Command::Join {
            guild: 1,
            channel: 10,
        });
        let status = h
            .voice_state(VoiceState::Retrying {
                attempt: 1,
                in_secs: 1,
            })
            .await;
        assert!(status.online().unwrap().notice.is_some());
        h.voice_state(VoiceState::Retrying {
            attempt: 2,
            in_secs: 2,
        })
        .await;
        let status = h.voice_state(VoiceState::Connected).await;
        assert!(status.online().unwrap().notice.is_none());

        // Lost later: retried from the first step again.
        h.backend.send(Event::VoiceLost {
            guild: 1,
            error: BotError::VoiceClosed(4015),
        });
        h.voice_state(VoiceState::Retrying {
            attempt: 1,
            in_secs: 1,
        })
        .await;
        h.voice_state(VoiceState::Connected).await;
    }

    #[tokio::test(start_paused = true)]
    async fn a_dave_refusal_is_not_retried() {
        let backend = FakeBackend::default();
        backend
            .joins
            .lock()
            .unwrap()
            .push_back(Err(BotError::from_close_code(4017)));
        let mut h = Harness::start_with(backend, SessionOptions::default());
        h.until("online", |s| s.online().is_some()).await;
        h.send(Command::Join {
            guild: 1,
            channel: 10,
        });
        let status = h
            .until("a notice", |s| {
                s.online()
                    .is_some_and(|o| o.notice.is_some() && o.voice.is_none())
            })
            .await;
        let notice = status.online().unwrap().notice.clone().unwrap();
        assert!(notice.text.contains("4017"), "{notice:?}");
        assert!(notice.hint.contains("needs an update"));
        assert_eq!(
            status.online().unwrap().input,
            DeviceStatus::Closed,
            "the device is closed again"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn being_kicked_leaves_for_good() {
        let mut h = Harness::start(SessionOptions::default());
        h.until("online", |s| s.online().is_some()).await;
        h.send(Command::Join {
            guild: 1,
            channel: 10,
        });
        h.voice_state(VoiceState::Connected).await;
        h.backend.send(Event::VoiceMoved {
            guild: 1,
            channel: Some(11),
            suppressed: false,
        });
        h.until("moved", |s| s.voice().is_some_and(|v| v.channel_id == 11))
            .await;
        h.backend.send(Event::VoiceMoved {
            guild: 1,
            channel: None,
            suppressed: false,
        });
        let status = h.until("out", |s| s.voice().is_none()).await;
        assert_eq!(
            status.online().unwrap().notice.as_ref().unwrap().text,
            BotError::Kicked.to_string()
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_failed_device_is_retried_while_in_a_channel() {
        let mut h = Harness::start(SessionOptions::default());
        *h.inputs.fail.lock().unwrap() = Some(AudioError::Disconnected);
        h.until("online", |s| s.online().is_some()).await;
        h.send(Command::Join {
            guild: 1,
            channel: 10,
        });
        let status = h.voice_state(VoiceState::Connected).await;
        assert!(matches!(
            status.online().unwrap().input,
            DeviceStatus::Failed { .. }
        ));
        // Three seconds later it opens.
        h.until("input open", |s| {
            s.online()
                .is_some_and(|o| matches!(o.input, DeviceStatus::Open { .. }))
        })
        .await;

        // Unplugged while running.
        h.backend.send(Event::InputFailed(AudioError::Disconnected));
        h.until("input failed", |s| {
            s.online()
                .is_some_and(|o| matches!(o.input, DeviceStatus::Failed { .. }))
        })
        .await;
        h.until("input open again", |s| {
            s.online()
                .is_some_and(|o| matches!(o.input, DeviceStatus::Open { .. }))
        })
        .await;
        assert_eq!(h.inputs.opened.lock().unwrap().len(), 2);
    }

    #[tokio::test(start_paused = true)]
    async fn the_input_can_change_in_a_channel() {
        let mut h = Harness::start(SessionOptions::default());
        h.until("online", |s| s.online().is_some()).await;
        h.send(Command::Join {
            guild: 1,
            channel: 10,
        });
        h.voice_state(VoiceState::Connected).await;
        h.send(Command::SetInput("input:other".into()));
        h.until("other input", |s| {
            s.online().is_some_and(|o| {
                o.input
                    == DeviceStatus::Open {
                        device: "Device input:other".into(),
                    }
            })
        })
        .await;
        // Still in the channel, joined once.
        assert!(h.status.borrow().voice().is_some());
        assert_eq!(
            h.backend
                .calls()
                .iter()
                .filter(|c| c.starts_with("join"))
                .count(),
            1
        );
    }

    #[tokio::test(start_paused = true)]
    async fn losing_the_gateway_fails_the_session() {
        let mut h = Harness::start(SessionOptions::default());
        h.until("online", |s| s.online().is_some()).await;
        h.backend.send(Event::GatewayLost(BotError::BadToken));
        h.until("failed", |s| matches!(s, BotStatus::Failed { .. }))
            .await;
        h.task.await.unwrap();
        assert_eq!(h.backend.calls(), ["login token", "logout"]);
    }

    /// Joins `channel` in "Home" and waits until connected.
    async fn joined(h: &mut Harness, channel: u64) {
        h.until("online", |s| s.online().is_some()).await;
        h.send(Command::Join { guild: 1, channel });
        h.voice_state(VoiceState::Connected).await;
    }

    fn listening(output: &str) -> ListenSettings {
        ListenSettings {
            enabled: true,
            output: output.into(),
            volume: 1.0,
        }
    }

    fn output_is(s: &BotStatus, device: &str) -> bool {
        s.online().is_some_and(|o| {
            o.output
                == DeviceStatus::Open {
                    device: device.into(),
                }
        })
    }

    #[tokio::test(start_paused = true)]
    async fn listening_opens_the_output_only_in_a_channel() {
        let mut h = Harness::start(SessionOptions {
            listen: listening("output:speakers"),
            ..SessionOptions::default()
        });
        let status = h.until("online", |s| s.online().is_some()).await;
        assert!(status.online().unwrap().listening);
        assert!(
            h.inputs.outputs.lock().unwrap().is_empty(),
            "nothing opens until a join"
        );

        joined(&mut h, 10).await;
        h.until("output open", |s| output_is(s, "Output output:speakers"))
            .await;
        assert!(h.backend.calls().contains(&"listening".to_owned()));

        // Another output: swapped in the channel.
        h.send(Command::SetListen(listening("output:headset")));
        h.until("other output", |s| output_is(s, "Output output:headset"))
            .await;

        // Off: closed, and decoding stops.
        h.send(Command::SetListen(ListenSettings::default()));
        h.until("not listening", |s| {
            s.online()
                .is_some_and(|o| !o.listening && o.output == DeviceStatus::Closed)
        })
        .await;
        assert!(h.backend.calls().contains(&"listen 1 false".to_owned()));
        assert_eq!(
            *h.inputs.outputs.lock().unwrap(),
            ["output:speakers", "output:headset"]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_failed_output_is_retried() {
        let mut h = Harness::start(SessionOptions::default());
        *h.inputs.fail_output.lock().unwrap() = Some(AudioError::Disconnected);
        joined(&mut h, 10).await;
        h.send(Command::SetListen(listening("output:x")));
        let status = h
            .until("output failed", |s| {
                s.online()
                    .is_some_and(|o| matches!(o.output, DeviceStatus::Failed { .. }))
            })
            .await;
        let notice = status.online().unwrap().notice.clone().unwrap();
        assert!(notice.text.starts_with("the output"), "{notice:?}");
        let status = h
            .until("output open", |s| output_is(s, "Output output:x"))
            .await;
        assert!(status.online().unwrap().notice.is_none());
    }

    fn follow(guild: u64, user: u64) -> BTreeMap<u64, ServerRules> {
        BTreeMap::from([(
            guild,
            ServerRules {
                auto_join: None,
                follow: Some(user),
            },
        )])
    }

    #[tokio::test(start_paused = true)]
    async fn follows_a_user_in_out_and_around() {
        let mut h = Harness::start(SessionOptions {
            servers: follow(1, 7),
            ..SessionOptions::default()
        });
        h.until("online", |s| s.online().is_some()).await;
        h.backend.send(Event::Ready);

        // Someone else: listed, not followed.
        h.backend.members.lock().unwrap().push((1, 8, 10));
        h.backend.send(Event::UserVoice {
            guild: 1,
            user: 8,
            channel: Some(10),
        });
        h.until("member listed", |s| {
            s.online().is_some_and(|o| o.servers[0].members.len() == 1)
        })
        .await;
        assert!(h.status.borrow().voice().is_none());

        // The followed user joins, then moves.
        h.backend.send(Event::UserVoice {
            guild: 1,
            user: 7,
            channel: Some(10),
        });
        h.until("in General", |s| {
            s.voice().is_some_and(|v| v.channel_id == 10)
        })
        .await;
        h.backend.send(Event::UserVoice {
            guild: 1,
            user: 7,
            channel: Some(11),
        });
        h.until("in Other", |s| {
            s.voice()
                .is_some_and(|v| v.channel_id == 11 && v.state == VoiceState::Connected)
        })
        .await;

        // Leaves: the bot leaves too, but stays online.
        h.backend.send(Event::UserVoice {
            guild: 1,
            user: 7,
            channel: None,
        });
        let status = h.until("out of voice", |s| s.voice().is_none()).await;
        assert!(status.online().is_some());
        let moves: Vec<String> = h
            .backend
            .calls()
            .into_iter()
            .filter(|c| c.starts_with("join") || c.starts_with("leave"))
            .collect();
        assert_eq!(
            moves,
            ["join 1 10 muted=true", "join 1 11 muted=true", "leave 1"]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn following_someone_already_in_voice_joins_them() {
        let mut h = Harness::start(SessionOptions::default());
        h.backend.members.lock().unwrap().push((2, 7, 20));
        h.until("online", |s| s.online().is_some()).await;
        h.send(Command::SetServerRules(follow(2, 7)));
        let status = h
            .until("in Lounge", |s| {
                s.voice()
                    .is_some_and(|v| v.guild_id == 2 && v.state == VoiceState::Connected)
            })
            .await;
        assert_eq!(status.voice().unwrap().channel_id, 20);
    }

    #[tokio::test(start_paused = true)]
    async fn after_login_follow_beats_auto_join_beats_last_channel() {
        let rules = |follow| {
            BTreeMap::from([
                (
                    1,
                    ServerRules {
                        auto_join: Some(11),
                        follow: None,
                    },
                ),
                (
                    2,
                    ServerRules {
                        auto_join: None,
                        follow,
                    },
                ),
            ])
        };
        // User 7 is in Lounge; user 99 isn't in voice.
        for (follow, expected) in [(Some(7), (2, 20)), (Some(99), (1, 11)), (None, (1, 11))] {
            let backend = FakeBackend::default();
            backend.members.lock().unwrap().push((2, 7, 20));
            let mut h = Harness::start_with(
                backend,
                SessionOptions {
                    servers: rules(follow),
                    rejoin: Some((1, 10)),
                    ..SessionOptions::default()
                },
            );
            h.until("online", |s| s.online().is_some()).await;
            h.backend.send(Event::Ready);
            let status = h.voice_state(VoiceState::Connected).await;
            let voice = status.voice().unwrap();
            assert_eq!((voice.guild_id, voice.channel_id), expected, "{follow:?}");
        }

        // An auto-join channel that no longer exists is skipped: the last channel.
        let mut h = Harness::start(SessionOptions {
            rejoin: Some((1, 10)),
            servers: BTreeMap::from([(
                1,
                ServerRules {
                    auto_join: Some(999),
                    follow: None,
                },
            )]),
            ..SessionOptions::default()
        });
        h.until("online", |s| s.online().is_some()).await;
        h.backend.send(Event::Ready);
        let status = h.voice_state(VoiceState::Connected).await;
        assert_eq!(status.voice().unwrap().channel_id, 10);
    }

    #[tokio::test(start_paused = true)]
    async fn on_a_stage_the_bot_asks_to_speak() {
        let backend = FakeBackend::default();
        *backend.stage.lock().unwrap() = Some(Ok(Stage::Requested));
        let mut h = Harness::start_with(backend, SessionOptions::default());
        joined(&mut h, 12).await;
        let status = h
            .until("requested", |s| {
                s.voice().is_some_and(|v| v.stage == Some(Stage::Requested))
            })
            .await;
        assert!(status.online().unwrap().notice.is_none());
        assert!(h.backend.calls().contains(&"stage 1 12".to_owned()));

        // Still in the audience: still waiting. Then a moderator accepts.
        for suppressed in [true, false] {
            h.backend.send(Event::VoiceMoved {
                guild: 1,
                channel: Some(12),
                suppressed,
            });
        }
        h.until("speaker", |s| {
            s.voice().is_some_and(|v| v.stage == Some(Stage::Speaker))
        })
        .await;
        // Moved back to the audience.
        h.backend.send(Event::VoiceMoved {
            guild: 1,
            channel: Some(12),
            suppressed: true,
        });
        h.until("audience", |s| {
            s.voice().is_some_and(|v| v.stage == Some(Stage::Audience))
        })
        .await;
    }

    #[tokio::test(start_paused = true)]
    async fn a_voice_channel_has_no_stage_state() {
        let mut h = Harness::start(SessionOptions::default());
        joined(&mut h, 10).await;
        assert_eq!(h.status.borrow().voice().unwrap().stage, None);
        assert!(!h.backend.calls().iter().any(|c| c.starts_with("stage")));
    }

    #[tokio::test(start_paused = true)]
    async fn a_refused_stage_request_shows_a_notice() {
        let backend = FakeBackend::default();
        *backend.stage.lock().unwrap() = Some(Err(BotError::Other("Missing Permissions".into())));
        let mut h = Harness::start_with(backend, SessionOptions::default());
        joined(&mut h, 12).await;
        let status = h
            .until("audience", |s| {
                s.voice().is_some_and(|v| v.stage == Some(Stage::Audience))
            })
            .await;
        let notice = status.online().unwrap().notice.clone().unwrap();
        assert!(notice.text.contains("stage"), "{notice:?}");
    }

    #[tokio::test(start_paused = true)]
    async fn leaving_a_server_leaves_its_channel_first() {
        let mut h = Harness::start(SessionOptions {
            servers: follow(1, 7),
            ..SessionOptions::default()
        });
        joined(&mut h, 10).await;
        h.send(Command::LeaveServer(1));
        let status = h
            .until("server gone", |s| {
                s.online().is_some_and(|o| o.server(1).is_none())
            })
            .await;
        assert!(status.voice().is_none());
        let calls = h.backend.calls();
        assert_eq!(&calls[calls.len() - 2..], ["leave 1", "leave server 1"]);
    }

    #[test]
    fn an_old_session_cannot_overwrite_a_newer_status() {
        let (tx, rx) = watch::channel(BotStatus::LoggingIn);
        let generation = Arc::new(AtomicU64::new(2));
        let old = StatusOut {
            tx: Arc::new(tx),
            generation,
            mine: 1,
        };
        old.set(BotStatus::Off);
        assert_eq!(*rx.borrow(), BotStatus::LoggingIn);
    }
}
