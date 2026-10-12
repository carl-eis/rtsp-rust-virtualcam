# 19 — Plan: Discord Tab

Status: plan (2026-10-12), nothing built. Branch `feat/audio-tab`. Refer to this work as
**"Discord Tab"**; phases are numbered D0–D8 so they don't clash with phases 0–8 in
[01](01-plan.md) or G/L/M in [11](11-virtual-cameras-linux-macos-plan.md).

The idea comes from [Discord Audio Stream Bot](https://github.com/BinkanSalaryman/Discord-Audio-Stream-Bot)
(Java, JDA 6.1, BASS, Swing; last commit 2025-11-10): a bot that sends a recording device's
sound into a Discord voice channel and can play the channel back. This plan builds the same
features in Rust, in a new tab in RTSP Cam's window.

Facts marked *check* are from documentation or memory and must be confirmed in D0 or in the
phase that relies on them.

## 1. Goal and limits

1. A **Discord** tab where the user pastes a bot token, logs in, picks a server and a voice
   channel, picks an input device, and the bot sends that sound into the channel.
2. Mute, a noise gate and a level meter in the tab. Settings are saved in `config.json`; the
   token is a `Secret` like stream passwords.
3. Later phases: hearing the channel (receive), auto-join, following a user, and a camera's
   own audio as the source.
4. Works on Windows and Linux. macOS builds and passes its tests (same level as the rest of
   the app, see [00 §1](00-index.md#1-where-the-project-stands-as-of-2026-10-10-v160)).
5. No OS `cfg` outside `rtspcam-platform` ([09 §3](09-cross-platform-plan.md#where-cfg-may-appear)).
   The camera features don't change, and a user who never opens the tab pays nothing at run
   time (no connection, no audio device opened).

Out of scope: user accounts ("selfbots", against Discord's terms; it has to be a bot account),
video or Go Live, music playback from files, and moderation features.

### Using the Java bot as a reference

The Java bot's source is public, and we use its logic freely: its audio format, frame timing,
lag handling, settings model and feature set are the starting point for this plan, and its
code is the first place to look when a question comes up ("how does it handle X?"). Because
this is a Rust rewrite on different libraries (songbird and cpal instead of JDA and BASS),
none of its code carries over directly. We learn from it and improve on it where we can; §2
lists what we keep and what we change.

(GitHub lists no licence file for the repo. That doesn't affect a rewrite that takes ideas and
behaviour rather than code.)

What is worth keeping from it:

- **The audio contract:** 48 kHz, stereo, 20 ms frames (960 samples per channel), the format
  Discord's Opus encoder wants, so no conversion on the send path once we're at 48 kHz.
- **A lag limit that throws audio away instead of falling behind:** above 200 ms queued, it
  clears the queue. Live sound should stay live; we keep the idea, but drop only the oldest
  blocks rather than everything (D1).
- **"Can provide" before "provide":** it only hands Discord a frame when a full 20 ms is
  queued, otherwise Discord sends nothing that tick. With songbird we return silence instead
  (D0 step 3), but the rule "never send a partial frame" stays.
- **One recording shared by several voice connections:** each connection gets its own queue
  fed from one capture callback. We need the same if the bot is ever in more than one
  channel (§7).
- **Starting muted**, and keeping mute separate from the device choice.
- **The settings model:** global (token, auto-login, devices, mute, threshold) plus per
  server (auto-join channel, followed user, command channels). D3 and D6 follow it.
- **Follow-a-user logic:** react to the followed member's voice state changes (join, move,
  leave) and mirror them.
- **Its known problems, as warnings:** the BASS handle that became invalid after moving
  between channels (fixed there by reopening the stream), and reading the global config from
  the audio callback. In Rust, the capture owns its state and settings reach it through a
  channel, so the callback never takes a lock it can wait on.

## 2. What the Java bot does, and what we change

| Feature | Java bot | This plan |
|---|---|---|
| Send audio | BASS records the device at 48 kHz stereo 16-bit; JDA pulls 20 ms frames; the queue is cleared above 200 ms of lag. | `cpal` capture, resampled to 48 kHz stereo `f32`, a bounded ring buffer that drops the oldest audio above a lag limit (D1). |
| Threshold | Each sample below the threshold is set to zero, which distorts quiet sound instead of gating it. | A real noise gate on 20 ms blocks (threshold in dBFS, hold, short fade) (D1). |
| Receive ("listen") | JDA's combined audio, played through BASS; the mixer divides by the number of handlers. | Songbird's per-speaker decoded audio, summed and soft-clipped, played with `cpal` (D5). |
| Join, leave | Slash commands `/join`, `/leave`, `/stop`, `/leave-all`. | Buttons in the tab (D4). Slash commands are deferred (§7). |
| Auto-join, follow a user | `/autojoin`, `/follow-audio`, per server. | Settings in the tab (D6). |
| Command channel binding, `/activity`, `/status`, `/stage` | Slash commands. | Deferred or dropped (§7). |
| Invite | Maintenance tab builds an invite link and lists servers to leave. | "Invite to a server" button (D4) and a server list with "Leave" (D6). |
| Token, settings | Gson file next to the jar, token in plain text. | `config.json` `discord` object, token sealed with `SecretStore` (D3). |
| Privileged intents | Needs SERVER MEMBERS (to check command permissions). | None, as long as there are no slash commands (*check* in D0). |

## 3. How it fits the code

```
rtspcam-app (Slint UI, Discord tab)
   ├── rtspcam-discord   NEW  serenity gateway + songbird voice (DAVE), bot state machine
   │      └── rtspcam-audio  NEW  cpal devices, capture/playback, resample, gate, meter, ring buffer
   └── rtspcam-core           `discord` settings, token as `Secret`
```

- **`rtspcam-audio`** has no Discord code, so D7 (camera audio) and tests can use it alone.
  `cpal` hides the OS (WASAPI, ALSA, CoreAudio), so no OS `cfg` is needed. If one turns out
  to be needed (for example WASAPI loopback), it goes in `rtspcam-platform` behind a small
  function, as 09 requires.
- **`rtspcam-discord`** runs on the app's existing tokio runtime (`manager.handle()`); it does
  not start its own. It exposes a `DiscordBot` handle with commands (`login`, `logout`,
  `join`, `leave`, `set_input`, `set_muted`, ...) and a `watch` channel of `BotStatus` that
  the UI reads, the same pattern as `CameraManager` and its status.
- The bot is **separate from `CameraManager`**: it doesn't reconcile streams, and a fault in
  one doesn't stop the other. Shutdown order in `main`: bot first (leave the channel, close
  the gateway, with a short limit), then the camera manager.

### Library choices (confirm in D0)

| Need | Choice | Why / notes |
|---|---|---|
| Voice, DAVE | `songbird` 0.6 | Since 2026-03-01 Discord refuses voice connections without DAVE (close code 4017). Songbird 0.6.0 (2026-04-05) supports DAVE and dropped the old `xsalsa20` modes. MSRV 1.83 (ours is 1.92). Uses `opus2` instead of `audiopus`. |
| Gateway, REST | `serenity` 0.12, default features off, only `gateway`, `cache`, `voice`, `rustls_backend` | No command framework needed. Alternative: `twilight` 0.17 (lighter, but we would write the guild/channel cache ourselves). *check* which serenity version songbird 0.6 pairs with. |
| Audio devices | `cpal` | Portable; WASAPI loopback (record what an output device plays) by opening an output device as input (*check* the cpal version). On Linux, PipeWire/PulseAudio "Monitor of ..." sources already show up as inputs through ALSA (*check*). |
| Resampling | `rubato`, only when the device is not 48 kHz | Most WASAPI devices default to 48 kHz; 44.1 kHz devices are common enough that we need it. |
| Ring buffer | `ringbuf` or our own `Mutex<VecDeque>` | The cpal callback must not block for long; measure before choosing. |

## 4. Phases

### D0. Spike (throwaway example, not merged)

A `examples/discord_spike.rs` in a scratch crate:

1. Log in with a test bot token, join a voice channel on a test server, play a 440 Hz tone
   through songbird. Confirm the DAVE handshake works (no 4017) with two human listeners in the
   channel, on Windows and Linux.
2. Replace the tone with `cpal` microphone capture. Note the latency by ear and the buffer
   sizes cpal gives on WASAPI and ALSA/PipeWire.
3. Feed songbird live PCM: find how to wrap a ring buffer as an `Input` (`RawAdapter` over a
   `Read` of `f32` *check*). On an underrun the reader must return **silence, not 0 bytes**,
   or songbird treats it as the end of the track (*check*).
4. Try WASAPI loopback (record the default output device). If it works, VB-Cable is no
   longer needed on Windows.
5. Receive: enable songbird's `receive` feature, log `VoiceTick` speakers for 30 s. Confirm
   decoded audio arrives with DAVE on (*check*).
6. Measure what it costs: new crates in `Cargo.lock`, clean release build time, binary size
   of `rtspcam.exe` before and after, and the native build needs of `opus2` (CMake? a C
   compiler is already required) on all three CI runners.
7. Confirm the bot needs no privileged intent for listing servers and voice channels
   (`GUILDS`, `GUILD_VOICE_STATES`).

Exit: a short "D0 results" section at the top of the D1 progress document with the answers,
and a go/no-go. If the build cost is too high, the fallback is putting the bot behind a cargo
feature `discord` (on by default in release, off in CI's quick jobs).

### D1. `rtspcam-audio` crate

1. `devices()`: inputs and outputs with a stable id (cpal name; *check* uniqueness when two
   devices share a name) and a "default" entry that follows the OS default.
2. `Capture`: opens an input device, converts to 48 kHz stereo `f32` (mono → both channels,
   more than two → first two, resample if needed), writes 20 ms blocks into a bounded buffer.
   Lag above a limit (200 ms, like the Java bot) drops the oldest blocks and counts them.
3. `Gate`: threshold in dBFS (default −50), 200 ms hold, 5 ms fade in/out, per 20 ms block.
   `Meter`: peak and RMS per block, for the UI.
4. `Playback` (used by D5): pulls 20 ms blocks from a buffer, silence on underrun.
5. Device loss (unplugged headset): the stream's error callback ends the capture and reports
   it; the caller reopens when the device comes back (poll `devices()` every few seconds).
6. Tests without hardware: a fake source feeding the converter, gate, meter and buffer
   (format conversion, resample length, gate open/close timing, lag drop).

### D2. `rtspcam-discord` crate

1. `DiscordBot::start(handle, settings)` and the command methods from §3. Internally one task
   owns the serenity `Client` and the songbird `Songbird` manager.
2. `BotStatus` (in a `watch`): `Off`, `LoggingIn`, `Online { user, servers }`, `InVoice
   { server, channel, speaking, lag_ms }`, `Failed { reason, hint }`. `servers` holds each
   server's voice and stage channels from the cache, updated on channel events.
3. Errors map to plain hints like the pipeline's `ErrorKind`: bad token ("The token was
   refused. Copy it again from the Developer Portal."), missing Connect/Speak permission,
   channel full, 4017 (would mean a songbird regression), network down.
4. Reconnect: serenity handles the gateway; on a voice disconnect that we didn't ask for,
   rejoin with backoff (1, 2, 5, 10, 30 s), the same shape as the pipeline's backoff.
5. Audio path: `Capture` → `Gate` → (muted? silence) → songbird input. Changing the device
   while in a channel swaps the capture without leaving.
6. Invite link: `https://discord.com/oauth2/authorize?client_id=<app id>&scope=bot&permissions=3146752`
   (View Channel 1024 + Connect 1048576 + Speak 2097152), app id from the REST "current
   application" call after login.
7. The token never reaches a log line: it is a `Secret` until it is handed to serenity, and
   serenity's own logging is checked for it (*check*; filter its target at `warn` if needed).
8. Tests: status transitions and error mapping with a fake gateway trait; no network in CI.

### D3. Config

1. A new optional top-level object in `config.json`, all fields with defaults so old files load
   unchanged (additive, so no migration and no version bump; *check* against the rules in
   [02 §3](02-progress.md#3-what-exists-crate-by-crate)):

   ```json
   "discord": {
     "token": "<sealed secret>",
     "auto_login": false,
     "input_device": "default",
     "muted": true,
     "gate": { "enabled": true, "threshold_db": -50 },
     "last_channel": { "guild_id": "...", "channel_id": "..." },
     "rejoin_last_channel": true
   }
   ```

2. `muted` starts `true`, so a new bot never broadcasts before the user means it to (the Java
   bot does the same).
3. Validation in `validate.rs` (threshold range, ids are numbers). Import/export of streams
   leaves the `discord` object out: it holds a token.
4. Headless mode (`--headless`): starts the bot when `auto_login` is on, so a server can run
   it without a window. Hand edits to `discord` apply live through the existing watcher.

### D4. The tab (UI)

1. `app.slint`: wrap the current content in a `TabWidget` with **Cameras** (today's window,
   unchanged) and **Discord**. The menu bar and status bar stay outside the tabs. The last
   tab is remembered in `AppSettings.extra` or a new field.
2. Discord tab, top to bottom:
   - **Bot**: token field (masked, "Paste" and "Show"), "Log in"/"Log out" button, status line
     with the bot's name, "Invite to a server" (opens the link in the browser),
     "Log in when RTSP Cam starts" checkbox, a link to the setup steps in the manual.
   - **Voice**: server and channel pickers (only voice and stage channels the bot can see),
     "Join"/"Leave".
   - **Sound**: input device picker (with a "Refresh" button), level meter with the gate
     threshold drawn on it, gate checkbox and threshold slider, a big **Mute** toggle.
3. New `src/ui/discord_tab.rs` following the pattern of the dialogs: it reads `BotStatus` and
   pushes it to the properties with `invoke_from_event_loop`; the meter updates at most 20
   times a second.
4. Tray menu: "Discord: Mute"/"Unmute" while in a channel.
5. Screenshot in `screenshots/` and a manual page `documentation/manual/discord.md` with the
   bot setup (create the application, add a bot, copy the token, invite it).

**D0–D4 is the first release.** It is useful on its own: stream a microphone, VB-Cable or
(if D0 step 4 works) the PC's own sound into a channel.

### D5. Hearing the channel (receive)

1. Songbird `receive` feature. Each `VoiceTick` gives 20 ms of decoded audio per speaker;
   sum them, soft-clip, and feed `Playback` on the chosen output device.
2. Tab: output device picker, "Play the channel" checkbox, volume slider. Off by default.
3. Don't play the bot's own sound back (songbird shouldn't deliver it; *check*).
4. Note in the manual that it plays everyone in the channel; recording it may need their
   consent where the user lives.

### D6. Automation and server management

1. Auto-join a chosen channel per server on login (the Java `/autojoin`).
2. Follow a user: when that user joins, moves or leaves a voice channel in a server, the bot
   does the same. Uses `GUILD_VOICE_STATES`, no privileged intent. Picked from the members
   currently in voice, or by user id.
3. Stage channels: join as a speaker when the bot has the permission, otherwise show that it
   has to be invited to speak (*check* songbird/serenity support for the suppress flag).
4. Server list with "Leave this server".

### D7. A camera's audio as the source (stretch, needs its own plan)

Many cameras send AAC or G.711 alongside the video, and `retina` can already receive it. A
source "Camera: Front Door" in the Sound picker would make this tab part of RTSP Cam rather
than a separate tool. It touches the pipeline (today video only), decoding (AAC through
`symphonia`, G.711 by hand), and the camera manager (an audio-only demand on a stream). Write
a separate plan before starting; this plan only keeps `rtspcam-audio`'s input side generic
(a trait for "something that produces 48 kHz stereo blocks") so it can plug in.

### D8. Packaging, CI, documentation

1. CI: `libasound2-dev` on Linux (cpal), anything `opus2` needs (from D0).
2. `.deb`/`.rpm`: add `libasound2` (`alsa-lib` on Fedora) to the hand-written requirements;
   `depends = "$auto"` covers the `.deb` (*check*).
3. Windows installer: nothing new if `opus2` links statically (*check*).
4. macOS: microphone access needs `NSMicrophoneUsageDescription` in an `Info.plist`, which
   only exists once there is an app bundle (11 phase M1). Until then macOS is "builds and
   tests pass".
5. README feature list and status table, this document's progress document (20 or later),
   rows in [00](00-index.md).

## 5. Order, size, and what each phase delivers

| Phase | Depends on | Size (rough) | Delivers |
|---|---|---|---|
| D0 | — | 1–2 days | Go/no-go, library versions, build cost |
| D1 | D0 | 2–3 days | `rtspcam-audio` with tests |
| D2 | D0, D1 | 3–4 days | `rtspcam-discord`: log in, join, send audio |
| D3 | D2 | 1 day | Saved settings, token sealed, headless support |
| D4 | D2, D3 | 2–3 days | The tab, tray item, manual page. **First release.** |
| D5 | D4 | 2 days | Hearing the channel |
| D6 | D4 | 2–3 days | Auto-join, follow a user, stage channels, leave server |
| D7 | D4 + own plan | 1–2 weeks | Camera audio into Discord |
| D8 | alongside D4, then each phase | spread out | CI, packages, docs |

## 6. Risks

| Risk | Effect | What we do |
|---|---|---|
| Songbird's DAVE code is six months old | Voice drops or fails on some server setups | D0 tests with real listeners on two OSes; pin the songbird version; status shows the close code so reports are useful. |
| Discord changes the voice protocol again | The tab stops working until songbird updates | Keep songbird easy to bump (one crate uses it); the camera features are unaffected. |
| Build cost (serenity, songbird, opus2) | Slower CI and builds, bigger binary | Measured in D0; fallback is the `discord` cargo feature. |
| The bot token is a full credential | A leaked token lets anyone control the bot | `Secret` sealed by the OS store, redacted `Debug`, never logged, left out of export. |
| Broadcasting by accident | The user's room goes into a channel | Starts muted, mute in the tray, the meter shows what is being sent. |
| Audio device differences (sample rates, PipeWire through ALSA, unplugging) | Glitches or silence | Resample in D1, reopen on device loss, lag limit with a counter in the status. |
| Off the product's main purpose | More to maintain for a side feature | Two self-contained crates and one tab; D7 ties it to cameras. |

## 7. Deferred or dropped

- **Slash commands** (`/join`, `/leave`, `/bind`, `/activity`, `/status`, `/about`, ...): the
  Java bot needs them because its window can't pick channels. Ours can. If added later, they
  bring back the SERVER MEMBERS intent for permission checks.
- **Several channels at once** (the Java bot can be in one channel per server and share one
  recording): possible with songbird, but the tab is designed for one channel; revisit after
  D4.
- **Music from files** and effects: not this app's job.
