//! One logged-in session: the state machine behind [`BotStatus`], independent of serenity and
//! songbird (they sit behind [`Backend`], so the logic is tested without a network).

use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use rtspcam_audio::{AudioError, BlockQueue, GateSettings};
use tokio::sync::{mpsc, watch};
use tokio::time::Instant;

use crate::audio::{AudioShared, Inputs};
use crate::error::BotError;
use crate::status::{BotStatus, InputStatus, Notice, Online, Server, VoiceState, VoiceStatus};

/// Waits between attempts to rejoin a channel after the connection dropped (the last one
/// repeats), the same shape as the camera pipeline's backoff.
const REJOIN_BACKOFF: [u64; 5] = [1, 2, 5, 10, 30];
/// A sound source that failed is tried again this often while in a channel.
const INPUT_RETRY: Duration = Duration::from_secs(3);
/// How often the lag shown in the status is brought up to date.
const STATUS_TICK: Duration = Duration::from_millis(500);

/// What a session starts with.
#[derive(Debug, Clone, PartialEq)]
pub struct SessionOptions {
    /// The sound source's device id (see `rtspcam_audio::DeviceInfo::id`).
    pub input: String,
    pub muted: bool,
    pub gate: GateSettings,
    /// A channel to join as soon as the bot is ready: (server id, channel id).
    pub rejoin: Option<(u64, u64)>,
}

impl Default for SessionOptions {
    fn default() -> Self {
        Self {
            input: rtspcam_audio::DEFAULT_DEVICE.to_owned(),
            muted: true,
            gate: GateSettings::default(),
            rejoin: None,
        }
    }
}

/// What the UI asks for.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Command {
    Join { guild: u64, channel: u64 },
    Leave,
    SetInput(String),
    SetMuted(bool),
    SetGate(GateSettings),
    Logout,
}

/// What the backend reports.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Event {
    /// The gateway session is ready.
    Ready,
    /// Servers or channels changed (the backend's cache has the new ones).
    ServersChanged,
    /// The voice connection is up (again).
    VoiceConnected {
        guild: u64,
    },
    /// The bot's own voice state changed: moved to `channel`, or out of voice (`None`).
    VoiceMoved {
        guild: u64,
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
    /// The servers and voice channels known now.
    fn servers(&self) -> Vec<Server>;
    /// Joins and starts sending the sound in `queue`.
    fn join(
        &mut self,
        guild: u64,
        channel: u64,
        queue: Arc<BlockQueue>,
        muted: bool,
    ) -> impl Future<Output = Result<(), BotError>> + Send;
    fn leave(&mut self, guild: u64) -> impl Future<Output = ()> + Send;
    fn set_muted(&mut self, guild: u64, muted: bool) -> impl Future<Output = ()> + Send;
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

pub(crate) struct Session<B, I> {
    backend: B,
    inputs: Arc<I>,
    out: StatusOut,
    shared: Arc<AudioShared>,
    options: SessionOptions,
    online: Online,
    events_tx: mpsc::UnboundedSender<Event>,
    events: mpsc::UnboundedReceiver<Event>,
    /// The channel the user wants the bot in.
    wanted: Option<(u64, u64)>,
    capture: Option<Box<dyn Send>>,
    /// When to try joining again, and the attempt number.
    rejoin_at: Option<(Instant, u32)>,
    input_retry_at: Option<Instant>,
    /// The ready event joins `options.rejoin` once.
    auto_joined: bool,
}

impl<B: Backend, I: Inputs> Session<B, I> {
    pub(crate) fn new(
        backend: B,
        inputs: Arc<I>,
        out: StatusOut,
        shared: Arc<AudioShared>,
        options: SessionOptions,
    ) -> Self {
        let (events_tx, events) = mpsc::unbounded_channel();
        Self {
            backend,
            inputs,
            out,
            shared,
            online: Online {
                muted: options.muted,
                ..Online::default()
            },
            options,
            events_tx,
            events,
            wanted: None,
            capture: None,
            rejoin_at: None,
            input_retry_at: None,
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
            let input_retry_at = self.input_retry_at;
            tokio::select! {
                command = commands.recv() => match command {
                    Some(Command::Logout) | None => break,
                    Some(command) => self.handle_command(command).await,
                },
                Some(event) = self.events.recv() => {
                    if let Event::GatewayLost(e) = &event {
                        tracing::warn!(error = %e, "the Discord connection ended");
                        self.close_capture();
                        self.backend.logout().await;
                        self.out.set(BotStatus::Failed { reason: e.to_string(), hint: e.hint() });
                        return;
                    }
                    self.handle_event(event).await;
                }
                () = sleep_until(rejoin_at) => self.retry_join().await,
                () = sleep_until(input_retry_at) => {
                    self.input_retry_at = None;
                    if self.wanted.is_some() && self.capture.is_none() {
                        self.open_capture().await;
                        self.publish();
                    }
                }
                _ = tick.tick() => self.update_lag(),
            }
        }

        tracing::info!("logging out of Discord");
        self.close_capture();
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
                    self.open_capture().await;
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
                    if let (Some((guild, channel)), None) = (self.options.rejoin, self.wanted) {
                        tracing::info!(guild, channel, "joining the last channel again");
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
            } => {
                if let Some((wanted_guild, wanted_channel)) = &mut self.wanted
                    && *wanted_guild == guild
                    && *wanted_channel != channel
                {
                    tracing::info!(guild, channel, "the bot was moved to another channel");
                    *wanted_channel = channel;
                    if let Some(voice) = self.voice_in(guild) {
                        voice.channel_id = channel;
                    }
                    self.publish();
                }
            }
            Event::VoiceMoved {
                guild,
                channel: None,
            } => {
                let connected = self
                    .voice_in(guild)
                    .is_some_and(|v| v.state == VoiceState::Connected);
                if connected {
                    self.give_up(guild, BotError::Kicked).await;
                }
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
                self.online.input = InputStatus::Failed {
                    reason: e.to_string(),
                };
                let e = BotError::Input(e);
                self.online.notice = Some(Notice {
                    text: e.to_string(),
                    hint: e.hint(),
                });
                if self.wanted.is_some() {
                    self.input_retry_at = Some(Instant::now() + INPUT_RETRY);
                }
                self.publish();
            }
            // Handled by `run`.
            Event::GatewayLost(_) => {}
        }
    }

    fn voice_in(&mut self, guild: u64) -> Option<&mut VoiceStatus> {
        self.online.voice.as_mut().filter(|v| v.guild_id == guild)
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
        });
        self.publish();
        if self.capture.is_none() {
            self.open_capture().await;
        }
        self.connect(guild, channel, 0).await;
    }

    /// One attempt at the voice connection; on failure either schedules the next one or gives
    /// up, depending on the error.
    async fn connect(&mut self, guild: u64, channel: u64, attempt: u32) {
        match self
            .backend
            .join(guild, channel, self.shared.queue.clone(), self.online.muted)
            .await
        {
            Ok(()) => {
                tracing::info!(guild, channel, "joined the voice channel");
                self.rejoin_at = None;
                self.online.notice = None;
                if let Some(voice) = self.voice_in(guild) {
                    voice.state = VoiceState::Connected;
                }
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

    fn forget_channel(&mut self) {
        self.wanted = None;
        self.rejoin_at = None;
        self.input_retry_at = None;
        self.online.voice = None;
        self.close_capture();
    }

    async fn open_capture(&mut self) {
        let inputs = self.inputs.clone();
        let id = self.options.input.clone();
        let sink = Box::new(self.shared.sink());
        let events = self.events_tx.clone();
        let on_error = Box::new(move |e| {
            let _ = events.send(Event::InputFailed(e));
        });
        let opened = tokio::task::spawn_blocking(move || inputs.open(&id, sink, on_error))
            .await
            .unwrap_or_else(|e| Err(AudioError::Other(e.to_string())));
        match opened {
            Ok((capture, device)) => {
                self.capture = Some(capture);
                self.online.input = InputStatus::Open { device };
                self.input_retry_at = None;
                if matches!(
                    &self.online.notice,
                    Some(n) if n.text.starts_with("the sound source")
                ) {
                    self.online.notice = None;
                }
            }
            Err(e) => {
                tracing::warn!(device = %self.options.input, error = %e, "could not open the sound source");
                self.online.input = InputStatus::Failed {
                    reason: e.to_string(),
                };
                let e = BotError::Input(e);
                self.online.notice = Some(Notice {
                    text: e.to_string(),
                    hint: e.hint(),
                });
                self.input_retry_at = Some(Instant::now() + INPUT_RETRY);
            }
        }
    }

    fn close_capture(&mut self) {
        if self.capture.take().is_some() {
            tracing::info!("sound source closed");
        }
        self.shared.reset_meter();
        if !matches!(self.online.input, InputStatus::Failed { .. }) {
            self.online.input = InputStatus::Closed;
        }
    }

    fn update_lag(&mut self) {
        let lag_ms = self.shared.queue.lag().as_millis() as u32;
        let dropped = self.shared.queue.dropped();
        let Some(voice) = self.online.voice.as_mut() else {
            return;
        };
        if voice.lag_ms != lag_ms || voice.dropped != dropped {
            voice.lag_ms = lag_ms;
            voice.dropped = dropped;
            self.publish();
        }
    }
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
    use crate::status::VoiceChannel;

    /// Records what it was asked to do; answers joins from a script (default: success).
    #[derive(Clone, Default)]
    struct FakeBackend {
        calls: Arc<Mutex<Vec<String>>>,
        login: Arc<Mutex<Option<BotError>>>,
        joins: Arc<Mutex<VecDeque<Result<(), BotError>>>>,
        events: Arc<Mutex<Option<mpsc::UnboundedSender<Event>>>>,
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

        fn servers(&self) -> Vec<Server> {
            vec![Server {
                id: 1,
                name: "Home".into(),
                channels: vec![VoiceChannel {
                    id: 10,
                    name: "General".into(),
                    stage: false,
                    can_speak: true,
                }],
            }]
        }

        async fn join(
            &mut self,
            guild: u64,
            channel: u64,
            _: Arc<BlockQueue>,
            muted: bool,
        ) -> Result<(), BotError> {
            self.log(format!("join {guild} {channel} muted={muted}"));
            self.joins.lock().unwrap().pop_front().unwrap_or(Ok(()))
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
    struct FakeInputs {
        opened: Mutex<Vec<String>>,
        fail: Mutex<Option<AudioError>>,
    }

    impl Inputs for FakeInputs {
        fn open(
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
        inputs: Arc<FakeInputs>,
        commands: mpsc::UnboundedSender<Command>,
        status: watch::Receiver<BotStatus>,
        task: tokio::task::JoinHandle<()>,
    }

    impl Harness {
        fn start(options: SessionOptions) -> Self {
            Self::start_with(FakeBackend::default(), options)
        }

        fn start_with(backend: FakeBackend, options: SessionOptions) -> Self {
            let inputs = Arc::new(FakeInputs::default());
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
            InputStatus::Open {
                device: "Device input:mic".into()
            }
        );
        assert_eq!(*h.inputs.opened.lock().unwrap(), ["input:mic"]);

        h.send(Command::SetMuted(false));
        h.until("unmuted", |s| s.online().is_some_and(|o| !o.muted))
            .await;

        h.send(Command::Leave);
        let status = h.until("out of voice", |s| s.voice().is_none()).await;
        assert_eq!(status.online().unwrap().input, InputStatus::Closed);
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
            InputStatus::Closed,
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
        });
        h.until("moved", |s| s.voice().is_some_and(|v| v.channel_id == 11))
            .await;
        h.backend.send(Event::VoiceMoved {
            guild: 1,
            channel: None,
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
            InputStatus::Failed { .. }
        ));
        // Three seconds later it opens.
        h.until("input open", |s| {
            s.online()
                .is_some_and(|o| matches!(o.input, InputStatus::Open { .. }))
        })
        .await;

        // Unplugged while running.
        h.backend.send(Event::InputFailed(AudioError::Disconnected));
        h.until("input failed", |s| {
            s.online()
                .is_some_and(|o| matches!(o.input, InputStatus::Failed { .. }))
        })
        .await;
        h.until("input open again", |s| {
            s.online()
                .is_some_and(|o| matches!(o.input, InputStatus::Open { .. }))
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
                    == InputStatus::Open {
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
