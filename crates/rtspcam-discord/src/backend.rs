//! The real [`Backend`]: serenity for the gateway and REST, songbird for voice (with DAVE).
//! The only module that knows either library exists.

use std::num::NonZeroU64;
use std::sync::Arc;

use async_trait::async_trait;
use rtspcam_audio::BlockQueue;
use serenity::all::{
    ChannelId, ChannelType, Client, Context, GatewayIntents, Guild, GuildChannel, GuildId, Http,
    Message, PartialGuild, Ready, ShardManager, UnavailableGuild, UserId,
    VoiceState as SerenityVoiceState,
};
use serenity::cache::Cache;
use serenity::client::EventHandler;
use serenity::json::json;
use songbird::driver::{Channels, DecodeConfig, DecodeMode, SampleRate};
use songbird::error::JoinError;
use songbird::events::context_data::DisconnectReason;
use songbird::tracks::PlayMode;
use songbird::{
    CoreEvent, Event as SongbirdEvent, EventContext, SerenityInit as _, Songbird, TrackEvent,
};
use tokio::sync::mpsc::UnboundedSender;
use tokio::task::JoinHandle;

use crate::audio::{ListenShared, sound_input};
use crate::error::BotError;
use crate::session::{Backend, Event, LoginInfo};
use crate::status::{Server, Stage, VoiceChannel, VoiceMember};

/// Servers, channels and the bot's own voice state; nothing privileged.
const INTENTS: GatewayIntents = GatewayIntents::GUILDS.union(GatewayIntents::GUILD_VOICE_STATES);

pub(crate) struct SerenityBackend {
    songbird: Arc<Songbird>,
    cache: Option<Arc<Cache>>,
    shards: Option<Arc<ShardManager>>,
    http: Option<Arc<Http>>,
    client: Option<JoinHandle<()>>,
    bot_id: Option<UserId>,
    events: Option<UnboundedSender<Event>>,
}

impl SerenityBackend {
    pub(crate) fn new() -> Self {
        Self {
            songbird: Songbird::serenity(),
            cache: None,
            shards: None,
            http: None,
            client: None,
            bot_id: None,
            events: None,
        }
    }
}

impl Backend for SerenityBackend {
    async fn login(
        &mut self,
        token: &str,
        events: UnboundedSender<Event>,
    ) -> Result<LoginInfo, BotError> {
        // A REST call first, so a bad token fails right away with a clear error instead of
        // inside the gateway task.
        let http = Http::new(token);
        let me = http.get_current_user().await.map_err(map_serenity)?;
        let app_id = match http.get_current_application_info().await {
            Ok(app) => Some(app.id.get()),
            Err(e) => {
                tracing::warn!(error = %e, "could not read the bot's application id");
                None
            }
        };
        self.bot_id = Some(me.id);
        self.events = Some(events.clone());

        let handler = Handler {
            events: events.clone(),
            bot_id: me.id,
        };
        let mut client = Client::builder(token, INTENTS)
            .event_handler(handler)
            .register_songbird_with(self.songbird.clone())
            .await
            .map_err(map_serenity)?;
        self.cache = Some(client.cache.clone());
        self.shards = Some(client.shard_manager.clone());
        self.http = Some(client.http.clone());
        self.client = Some(tokio::spawn(async move {
            if let Err(e) = client.start().await {
                let _ = events.send(Event::GatewayLost(map_serenity(e)));
            }
        }));
        Ok(LoginInfo {
            user: me.name.clone(),
            app_id,
        })
    }

    fn servers(&self) -> Vec<Server> {
        let Some(cache) = &self.cache else {
            return Vec::new();
        };
        let mut servers: Vec<Server> = cache
            .guilds()
            .into_iter()
            .filter_map(|id| {
                let guild = cache.guild(id)?;
                Some(server_of(&guild, self.bot_id))
            })
            .collect();
        servers.sort_by_key(|s| s.name.to_lowercase());
        servers
    }

    async fn join(
        &mut self,
        guild: u64,
        channel: u64,
        queue: Arc<BlockQueue>,
        muted: bool,
        listen: Arc<ListenShared>,
    ) -> Result<(), BotError> {
        let (Some(guild_id), Some(channel_id)) = (NonZeroU64::new(guild), NonZeroU64::new(channel))
        else {
            return Err(BotError::Other("invalid server or channel id".into()));
        };
        let call = match self.songbird.join(guild_id, channel_id).await {
            Ok(call) => call,
            Err(e) => {
                // Don't leave a half-made call behind.
                let _ = self.songbird.remove(guild_id).await;
                return Err(map_join(&e));
            }
        };
        let mut call = call.lock().await;
        call.remove_all_global_events();
        let events = VoiceEvents {
            events: self
                .events
                .clone()
                .ok_or_else(|| BotError::Other("not logged in".into()))?,
            guild,
        };
        for event in [
            CoreEvent::DriverConnect,
            CoreEvent::DriverReconnect,
            CoreEvent::DriverDisconnect,
        ] {
            call.add_global_event(SongbirdEvent::Core(event), events.clone());
        }
        let decode = listen.enabled();
        call.add_global_event(
            SongbirdEvent::Core(CoreEvent::VoiceTick),
            ListenEvents { listen },
        );
        set_decoding(&mut call, decode);
        if let Err(e) = call.mute(muted).await {
            tracing::warn!(error = %e, "could not set the bot's mute state");
        }
        let track = call.play_only_input(sound_input(queue));
        if let Err(e) = track.add_event(
            SongbirdEvent::Track(TrackEvent::Error),
            TrackErrors {
                events: events.events.clone(),
                guild,
            },
        ) {
            tracing::debug!(error = %e, "could not watch the sound track");
        }
        Ok(())
    }

    async fn leave(&mut self, guild: u64) {
        if let Some(id) = NonZeroU64::new(guild)
            && let Err(e) = self.songbird.remove(id).await
        {
            tracing::debug!(error = %e, "leaving the voice channel");
        }
    }

    async fn set_muted(&mut self, guild: u64, muted: bool) {
        let Some(call) = NonZeroU64::new(guild).and_then(|id| self.songbird.get(id)) else {
            return;
        };
        // Muting the call stops the sound and shows the bot as muted in the channel; songbird
        // sends a few silent frames first, so listeners' decoders don't repeat the last sound.
        if let Err(e) = call.lock().await.mute(muted).await {
            tracing::warn!(error = %e, "could not change the bot's mute state");
        }
    }

    async fn set_listening(&mut self, guild: u64, on: bool) {
        if let Some(call) = NonZeroU64::new(guild).and_then(|id| self.songbird.get(id)) {
            set_decoding(&mut *call.lock().await, on);
        }
    }

    async fn speak_on_stage(&mut self, guild: u64, channel: u64) -> Result<Stage, BotError> {
        let http = self.http()?;
        let guild = GuildId::new(guild);
        let channel = ChannelId::new(channel);
        // Becoming a speaker needs Mute Members; without it, ask to speak instead.
        let speak = json!({ "channel_id": channel, "suppress": false });
        match http.edit_voice_state_me(guild, &speak).await {
            Ok(()) => return Ok(Stage::Speaker),
            Err(e) => tracing::info!(error = %e, "can't become a stage speaker; asking to speak"),
        }
        let ask = json!({
            "channel_id": channel,
            "request_to_speak_timestamp": serenity::model::Timestamp::now(),
        });
        http.edit_voice_state_me(guild, &ask)
            .await
            .map_err(map_serenity)?;
        Ok(Stage::Requested)
    }

    async fn leave_server(&mut self, guild: u64) -> Result<(), BotError> {
        let http = self.http()?;
        GuildId::new(guild).leave(&http).await.map_err(map_serenity)
    }

    async fn logout(&mut self) {
        let guilds: Vec<_> = self.songbird.iter().map(|(id, _)| id).collect();
        for guild in guilds {
            let _ = self.songbird.remove(guild).await;
        }
        if let Some(shards) = self.shards.take() {
            shards.shutdown_all().await;
        }
        if let Some(client) = self.client.take() {
            client.abort();
        }
        self.cache = None;
        self.http = None;
    }
}

impl SerenityBackend {
    fn http(&self) -> Result<Arc<Http>, BotError> {
        self.http
            .clone()
            .ok_or_else(|| BotError::Other("not logged in".into()))
    }
}

/// Decodes what is heard (to play the channel), or only decrypts it, which costs next to
/// nothing, while not listening.
fn set_decoding(call: &mut songbird::Call, on: bool) {
    let mode = if on {
        DecodeMode::Decode(DecodeConfig::new(Channels::Stereo, SampleRate::Hz48000))
    } else {
        DecodeMode::Decrypt
    };
    if call.config().decode_mode != mode {
        let config = call.config().clone().decode_mode(mode);
        call.set_config(config);
    }
}

fn server_of(guild: &Guild, bot_id: Option<UserId>) -> Server {
    let me = bot_id.and_then(|id| guild.members.get(&id));
    let mut channels: Vec<&GuildChannel> = guild
        .channels
        .values()
        .filter(|c| matches!(c.kind, ChannelType::Voice | ChannelType::Stage))
        .collect();
    channels.sort_by_key(|c| (c.position, c.id));
    let mut members: Vec<VoiceMember> = guild
        .voice_states
        .values()
        .filter(|v| Some(v.user_id) != bot_id)
        .filter_map(|v| {
            let channel = v.channel_id?;
            let name = guild
                .members
                .get(&v.user_id)
                .or(v.member.as_ref())
                .map_or_else(|| v.user_id.to_string(), |m| m.display_name().to_owned());
            Some(VoiceMember {
                id: v.user_id.get(),
                name,
                channel_id: channel.get(),
            })
        })
        .collect();
    members.sort_by_key(|m| m.name.to_lowercase());
    Server {
        id: guild.id.get(),
        name: guild.name.clone(),
        members,
        channels: channels
            .into_iter()
            .map(|c| {
                let stage = c.kind == ChannelType::Stage;
                // Unknown without the bot's member entry: let the join find out.
                let can_speak = me.is_none_or(|member| {
                    let p = guild.user_permissions_in(c, member);
                    p.view_channel() && p.connect() && (stage || p.speak())
                });
                VoiceChannel {
                    id: c.id.get(),
                    name: c.name.clone(),
                    stage,
                    can_speak,
                }
            })
            .collect(),
    }
}

fn map_serenity(e: serenity::Error) -> BotError {
    use serenity::gateway::GatewayError;
    match &e {
        serenity::Error::Http(http) => match http.status_code() {
            Some(status) => BotError::from_http_status(status.as_u16(), e.to_string()),
            None => BotError::Network(e.to_string()),
        },
        serenity::Error::Gateway(GatewayError::InvalidAuthentication) => BotError::BadToken,
        serenity::Error::Gateway(
            GatewayError::InvalidGatewayIntents | GatewayError::DisallowedGatewayIntents,
        ) => BotError::Intents,
        serenity::Error::Gateway(_) | serenity::Error::Tungstenite(_) | serenity::Error::Io(_) => {
            BotError::Network(e.to_string())
        }
        _ => BotError::Other(e.to_string()),
    }
}

fn map_join(e: &JoinError) -> BotError {
    match e {
        JoinError::TimedOut => BotError::JoinTimedOut,
        JoinError::Driver(connection) => match DisconnectReason::from(connection) {
            DisconnectReason::WsClosed(Some(code)) => BotError::from_close_code(code as u16),
            DisconnectReason::TimedOut => BotError::JoinTimedOut,
            DisconnectReason::Io => BotError::Network(connection.to_string()),
            _ => BotError::Voice(connection.to_string()),
        },
        _ => BotError::Voice(e.to_string()),
    }
}

/// Gateway events, passed on to the session.
struct Handler {
    events: UnboundedSender<Event>,
    bot_id: UserId,
}

impl Handler {
    fn send(&self, event: Event) {
        let _ = self.events.send(event);
    }
}

#[async_trait]
impl EventHandler for Handler {
    async fn ready(&self, _: Context, ready: Ready) {
        tracing::info!(servers = ready.guilds.len(), "Discord gateway ready");
        // The servers arrive after this, one by one; `cache_ready` says when all have.
        if ready.guilds.is_empty() {
            self.send(Event::Ready);
        } else {
            self.send(Event::ServersChanged);
        }
    }

    async fn cache_ready(&self, _: Context, _: Vec<GuildId>) {
        self.send(Event::Ready);
    }

    async fn guild_create(&self, _: Context, _: Guild, _: Option<bool>) {
        self.send(Event::ServersChanged);
    }

    async fn guild_update(&self, _: Context, _: Option<Guild>, _: PartialGuild) {
        self.send(Event::ServersChanged);
    }

    async fn guild_delete(&self, _: Context, _: UnavailableGuild, _: Option<Guild>) {
        self.send(Event::ServersChanged);
    }

    async fn channel_create(&self, _: Context, _: GuildChannel) {
        self.send(Event::ServersChanged);
    }

    async fn channel_update(&self, _: Context, _: Option<GuildChannel>, _: GuildChannel) {
        self.send(Event::ServersChanged);
    }

    async fn channel_delete(&self, _: Context, _: GuildChannel, _: Option<Vec<Message>>) {
        self.send(Event::ServersChanged);
    }

    async fn voice_state_update(
        &self,
        _: Context,
        _: Option<SerenityVoiceState>,
        new: SerenityVoiceState,
    ) {
        let Some(guild) = new.guild_id else {
            return;
        };
        let channel = new.channel_id.map(|c| c.get());
        if new.user_id == self.bot_id {
            self.send(Event::VoiceMoved {
                guild: guild.get(),
                channel,
                suppressed: new.suppress,
            });
        } else {
            self.send(Event::UserVoice {
                guild: guild.get(),
                user: new.user_id.get(),
                channel,
            });
        }
    }
}

/// The track that sends the sound failed: reported as a voice error, so the session shows it
/// and joins again (which makes a new track).
struct TrackErrors {
    events: UnboundedSender<Event>,
    guild: u64,
}

#[async_trait]
impl songbird::EventHandler for TrackErrors {
    async fn act(&self, ctx: &EventContext<'_>) -> Option<SongbirdEvent> {
        if let EventContext::Track(tracks) = ctx {
            for (state, _) in *tracks {
                if let PlayMode::Errored(e) = &state.playing {
                    tracing::warn!(guild = self.guild, error = %e, "the sound track failed");
                    let _ = self.events.send(Event::VoiceLost {
                        guild: self.guild,
                        error: BotError::Voice(format!("the sound could not be sent ({e})")),
                    });
                }
            }
        }
        None
    }
}

/// What is heard in the channel: each 20 ms tick, the decoded sound of everyone speaking.
struct ListenEvents {
    listen: Arc<ListenShared>,
}

#[async_trait]
impl songbird::EventHandler for ListenEvents {
    async fn act(&self, ctx: &EventContext<'_>) -> Option<SongbirdEvent> {
        if let EventContext::VoiceTick(tick) = ctx {
            self.listen.tick(
                tick.speaking
                    .values()
                    .filter_map(|v| v.decoded_voice.as_deref()),
            );
        }
        None
    }
}

/// Voice connection events for one server, passed on to the session.
#[derive(Clone)]
struct VoiceEvents {
    events: UnboundedSender<Event>,
    guild: u64,
}

#[async_trait]
impl songbird::EventHandler for VoiceEvents {
    async fn act(&self, ctx: &EventContext<'_>) -> Option<SongbirdEvent> {
        let event = match ctx {
            EventContext::DriverConnect(_) | EventContext::DriverReconnect(_) => {
                Some(Event::VoiceConnected { guild: self.guild })
            }
            EventContext::DriverDisconnect(data) => match data.reason {
                // Asked for (leaving, or a channel change): nothing to do.
                None | Some(DisconnectReason::Requested | DisconnectReason::AttemptDiscarded) => {
                    None
                }
                Some(DisconnectReason::WsClosed(Some(code))) => {
                    Some(BotError::from_close_code(code as u16))
                }
                Some(DisconnectReason::WsClosed(None) | DisconnectReason::Io) => {
                    Some(BotError::Network("the voice connection dropped".into()))
                }
                Some(DisconnectReason::TimedOut) => Some(BotError::JoinTimedOut),
                Some(other) => Some(BotError::Voice(format!("{other:?}"))),
            }
            .map(|error| Event::VoiceLost {
                guild: self.guild,
                error,
            }),
            _ => None,
        };
        if let Some(event) = event {
            let _ = self.events.send(event);
        }
        None
    }
}
