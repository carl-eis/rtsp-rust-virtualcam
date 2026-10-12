use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use rtspcam_audio::{AudioError, DeviceInfo, GateSettings};
use tokio::runtime::Runtime;
use tokio::sync::{mpsc, oneshot, watch};

use crate::audio::{AudioShared, DeviceInputs};
use crate::backend::SerenityBackend;
use crate::session::{Command, Session, SessionOptions, StatusOut};
use crate::status::{BotStatus, Meter};

/// Worker threads of the bot's runtime: the gateway, voice and the session are light.
const WORKER_THREADS: usize = 2;
/// A logout that hasn't finished in this long is cut short.
const LOGOUT_LIMIT: Duration = Duration::from_secs(3);

/// The Discord bot, as the app sees it: commands in, [`BotStatus`] out.
///
/// Creating it starts nothing. The first [`login`](Self::login) creates the bot's own tokio
/// runtime (named `discord` threads, separate from the cameras'), and logging out drops it, so
/// a user who never logs in runs no Discord code at all (documentation/19 §3, "Keeping the
/// cameras safe"). All methods return at once; work happens on the runtime.
pub struct DiscordBot {
    status: Arc<watch::Sender<BotStatus>>,
    /// Bumped by each login; an older session's late status updates are ignored.
    generation: Arc<AtomicU64>,
    /// Applied at the next login; sent to the running session as they change.
    options: Mutex<SessionOptions>,
    running: Mutex<Option<Running>>,
}

struct Running {
    runtime: Runtime,
    commands: mpsc::UnboundedSender<Command>,
    done: oneshot::Receiver<()>,
    shared: Arc<AudioShared>,
}

impl std::fmt::Debug for DiscordBot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DiscordBot")
            .field("status", &*self.status.borrow())
            .finish_non_exhaustive()
    }
}

impl Default for DiscordBot {
    fn default() -> Self {
        Self::new(SessionOptions::default())
    }
}

impl DiscordBot {
    pub fn new(options: SessionOptions) -> Self {
        Self {
            status: Arc::new(watch::channel(BotStatus::Off).0),
            generation: Arc::new(AtomicU64::new(0)),
            options: Mutex::new(options),
            running: Mutex::new(None),
        }
    }

    fn running(&self) -> MutexGuard<'_, Option<Running>> {
        self.running.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn options(&self) -> MutexGuard<'_, SessionOptions> {
        self.options.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The status, to read now or to wait for changes on.
    pub fn status(&self) -> watch::Receiver<BotStatus> {
        self.status.subscribe()
    }

    /// The sound going out (for the level meter), while a sound source is open.
    pub fn meter(&self) -> Option<Meter> {
        let running = self.running();
        let running = running.as_ref()?;
        let online = self.status.borrow();
        let open = matches!(
            online.online().map(|o| &o.input),
            Some(crate::status::InputStatus::Open { .. })
        );
        open.then(|| running.shared.meter())
    }

    /// Logs in with `token` (logging out of an earlier session first). `rejoin` is a channel
    /// to join once ready: (server id, channel id).
    pub fn login(&self, token: String, rejoin: Option<(u64, u64)>) {
        if let Some(old) = self.running().take() {
            stop_in_background(old);
        }
        let runtime = match tokio::runtime::Builder::new_multi_thread()
            .worker_threads(WORKER_THREADS)
            .thread_name("discord")
            .enable_all()
            .build()
        {
            Ok(runtime) => runtime,
            Err(e) => {
                self.status.send_replace(BotStatus::Failed {
                    reason: format!("could not start the bot: {e}"),
                    hint: "Try again.".into(),
                });
                return;
            }
        };
        let generation = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
        let mut options = self.options().clone();
        options.rejoin = rejoin;
        let shared = Arc::new(AudioShared::new(options.muted, options.gate));
        let out = StatusOut {
            tx: self.status.clone(),
            generation: self.generation.clone(),
            mine: generation,
        };
        let session = Session::new(
            SerenityBackend::new(),
            Arc::new(DeviceInputs),
            out,
            shared.clone(),
            options,
        );
        let (commands, commands_rx) = mpsc::unbounded_channel();
        let (done_tx, done) = oneshot::channel();
        runtime.spawn(async move {
            session.run(token, commands_rx).await;
            let _ = done_tx.send(());
        });
        *self.running() = Some(Running {
            runtime,
            commands,
            done,
            shared,
        });
    }

    /// Leaves the channel, closes the connection and stops the bot's runtime, in the
    /// background.
    pub fn logout(&self) {
        if let Some(running) = self.running().take() {
            self.finish();
            stop_in_background(running);
        }
    }

    /// Like [`logout`](Self::logout), but waits (at most `limit`). For quitting the app.
    pub fn shutdown(&self, limit: Duration) {
        if let Some(running) = self.running().take() {
            self.finish();
            stop(running, limit);
        }
    }

    /// The session is ending: the status says so at once, and the session's last updates
    /// while it leaves are ignored.
    fn finish(&self) {
        self.generation.fetch_add(1, Ordering::SeqCst);
        self.status.send_replace(BotStatus::Off);
    }

    fn send(&self, command: Command) {
        if let Some(running) = self.running().as_ref() {
            let _ = running.commands.send(command);
        }
    }

    pub fn join(&self, guild: u64, channel: u64) {
        self.send(Command::Join { guild, channel });
    }

    pub fn leave(&self) {
        self.send(Command::Leave);
    }

    /// The sound source (an id from [`list_devices`]).
    pub fn set_input(&self, id: &str) {
        self.options().input = id.to_owned();
        self.send(Command::SetInput(id.to_owned()));
    }

    pub fn set_muted(&self, muted: bool) {
        self.options().muted = muted;
        self.send(Command::SetMuted(muted));
    }

    pub fn set_gate(&self, gate: GateSettings) {
        self.options().gate = gate;
        self.send(Command::SetGate(gate));
    }
}

impl Drop for DiscordBot {
    fn drop(&mut self) {
        if let Some(running) = self.running().take() {
            stop(running, LOGOUT_LIMIT);
        }
    }
}

fn stop_in_background(running: Running) {
    let spawned = std::thread::Builder::new()
        .name("discord-stop".into())
        .spawn(move || stop(running, LOGOUT_LIMIT));
    if let Err(e) = spawned {
        tracing::warn!(error = %e, "could not stop the Discord bot in the background");
    }
}

/// Asks the session to log out, waits for it (at most `limit`), then drops the runtime.
fn stop(running: Running, limit: Duration) {
    let Running {
        runtime,
        commands,
        done,
        ..
    } = running;
    let _ = commands.send(Command::Logout);
    let finished = runtime.block_on(async { tokio::time::timeout(limit, done).await.is_ok() });
    if !finished {
        tracing::warn!("the Discord bot didn't log out in time; stopping it");
    }
    runtime.shutdown_timeout(Duration::from_millis(500));
}

/// Lists the devices that can be a sound source, on a worker thread (never the UI thread: on
/// Windows it sets up COM), and hands the result to `reply` on that thread. Input devices
/// first; then output devices where the OS can record what they play.
pub fn list_sources(reply: impl FnOnce(Result<Vec<DeviceInfo>, AudioError>) + Send + 'static) {
    let spawned = std::thread::Builder::new()
        .name("audio-devices".into())
        .spawn(move || {
            let loopback = rtspcam_audio::loopback_supported();
            reply(rtspcam_audio::devices().map(|list| {
                list.into_iter()
                    .filter(|d| loopback || d.kind == rtspcam_audio::DeviceKind::Input)
                    .collect()
            }));
        });
    if let Err(e) = spawned {
        tracing::warn!(error = %e, "could not list audio devices");
    }
}

/// The link that adds the bot (application `app_id`) to a server, asking for View Channel,
/// Connect and Speak.
pub fn invite_url(app_id: u64) -> String {
    format!(
        "https://discord.com/oauth2/authorize?client_id={app_id}&scope=bot&permissions={INVITE_PERMISSIONS}"
    )
}

/// View Channel (1 << 10), Connect (1 << 20), Speak (1 << 21).
pub const INVITE_PERMISSIONS: u64 = (1 << 10) | (1 << 20) | (1 << 21);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invite_permissions_are_view_connect_speak() {
        assert_eq!(INVITE_PERMISSIONS, 3_146_752);
        assert!(invite_url(123).contains("client_id=123&scope=bot&permissions=3146752"));
    }

    #[test]
    fn a_new_bot_starts_nothing() {
        let bot = DiscordBot::default();
        assert_eq!(*bot.status().borrow(), BotStatus::Off);
        assert!(bot.running().is_none());
        assert!(bot.meter().is_none());
        // Commands without a session are kept for the next login, not lost or panicking.
        bot.set_muted(false);
        bot.set_input("input:x");
        bot.join(1, 2);
        assert!(!bot.options().muted);
        assert_eq!(bot.options().input, "input:x");
        bot.logout();
        bot.shutdown(Duration::from_millis(10));
    }
}
