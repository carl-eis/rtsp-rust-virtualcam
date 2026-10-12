# 21 — Progress: Discord Tab, phases D5, D6 and D8

Date: 2026-10-12. Branch `feat/audio-tab`. Continues [20](20-progress-discord-tab.md) (D0–D4)
with phases D5 (hearing the channel), D6 (auto-join, follow a user, stage channels, leaving a
server) and D8 (packaging, CI, documentation) of [19](19-discord-tab-plan.md). D7 (a camera's
audio as the source) needs its own plan and is not started.

This is also the first session with a bot token and a test server, so it starts with what was
checked against Discord, including a bug in D2 that only showed up there.

## 1. Checked against Discord

A bot ("StudioBot") in one server, Windows 11, from `examples/bot_check.rs` (new, §4) and from
the app itself.

| Check (20 §5) | Result |
|---|---|
| Log in, bot name, invite link | Works. The REST check, the gateway, and the invite link with the app id. |
| Servers and voice channels | Works with only `GUILDS` and `GUILD_VOICE_STATES`: 7 channels listed, and the one the bot's role can't speak in is marked. |
| Joining, DAVE | Works: joined (Aes256Gcm), the DAVE handshake ran (binary opcode 25 received), **no 4017**. |
| Microphone opens on join, closes on leave | Works (USB headset, 48 kHz stereo). |
| **Sending sound** | **Broken in D0–D4, fixed here** (§2). Now the track reaches `Playable`. Not yet heard by a person. |
| People in voice, with names | Works without the Server Members intent: the member entries come with the voice states. |
| Follow at login (D6) | Works: with a follow rule for someone already in voice, the bot joined their channel after logging in, in `bot_check` and in the app with auto-login. |
| Hearing the channel (D5) | The output opens on join and decoding switches on; **not yet heard**: nobody spoke in the channel during the tests. |
| The app, logged in | Auto-login, follow, capture and playback together, with the cameras running ("2 cameras active", no errors). |
| A bad token | Already checked in D4: "Discord refused the token" with its hint. |

In debug builds serenity-voice-model prints every binary voice message to stderr (`[DAVE
Binary] Received ...`, an `eprintln!` under `cfg(debug_assertions)`). Release builds don't.

## 2. Bug: the bot joined but sent nothing (D2)

Right after joining, songbird reported the sending track as `Preparing`, then `End` and
`Error`. A track error handler (new, below) logged the reason:

```
runtime error while playing track: parsing formats/codecs [malformed stream: no compatible track found]
```

`RawAdapter` (our live sound, see 20 D0 step 3) wraps the stream as 32-bit float PCM, and
songbird decodes it with symphonia's PCM codec. Songbird leaves symphonia's codec features to
the application, and none was enabled, so the codec registry had no PCM decoder and the track
failed before sending a single frame. D2's tests didn't catch it because they use a fake
backend, and the D4 test only tried a bad token.

Fixes:

- `rtspcam-discord` depends on `symphonia` 0.5 with only the `pcm` feature (feature
  unification turns it on for songbird). `Cargo.lock` gains `symphonia-codec-pcm`.
- A test, `audio::tests::songbird_can_play_the_queue`, prepares the input with songbird's own
  codec registry and probe, as the driver does. It fails without the feature.
- The input is made in one place (`audio::sound_input`).
- A failed sending track is now reported: it is logged, and sent to the session as a voice
  error, which shows "the sound could not be sent" with a hint and joins again (which makes a
  new track), with the usual backoff.

## 3. What was done

### D5 Hearing the channel

- songbird's `receive` feature. Each `VoiceTick` (every 20 ms) gives each speaker's decoded
  sound (48 kHz stereo `i16`). `ListenShared` (in `audio.rs`) mixes them with the volume,
  soft-clips, and queues one block for `Playback`; nothing is queued while nobody speaks (the
  playback plays silence).
- `rtspcam-audio`: `Mix` (sums voices with a gain) and `soft_clip` (unchanged up to 0.8, then a
  tanh curve towards 1.0), with tests.
- Decoding costs CPU, so the call decodes only while listening (`DecodeMode::Decode`), and
  only decrypts otherwise. Switching it on or off changes the call's config without rejoining.
- The output opens only while the bot is in a channel and listening, on the bot's runtime like
  the capture (a `Devices` trait now opens inputs and outputs). A failed or unplugged output is
  shown ("the output for hearing the channel failed") and retried every 3 s, like the source;
  changing the output swaps it in the channel.
- `Online` has `listening` and `output`; `VoiceStatus` has `speakers` (people heard just now,
  updated twice a second), which the tab shows as "N people talking".
- The bot's own sound: Discord doesn't send it back, so there is nothing to filter.
- `discord.json`: `listen { enabled, output_device, volume_percent }`, off by default, volume
  0–200 %.
- Tab: a **Hear the channel** group (checkbox and output picker, volume slider, the output's
  status, and the consent note from 19 §4 D5 step 4). The device list now gives sources and
  outputs (`list_devices` replaces `list_sources`).

### D6 Automation and server management

- Per server rules, `ServerRules { auto_join, follow }`, stored in `discord.json` as `servers`,
  keyed by server id (ids as strings, like `last_channel`). Validation checks every id.
- The bot is in one channel at a time (19 §7), so after logging in it picks: a followed person
  already in voice, then an auto-join channel (servers in the listed order, channels that no
  longer exist skipped), then the last channel. This runs once, when the servers are known:
  `cache_ready` now sends `Ready` (the gateway's own ready event comes before the servers;
  a bot in no server gets `Ready` from it directly).
- Follow: other people's voice state updates arrive as `UserVoice`; when the followed person
  joins or moves, the bot joins them (even from another server), and when they leave, the bot
  leaves and stays online. Choosing someone to follow who is in voice now joins them at once.
- `Server.members`: the people in the server's voice channels, by server nickname, display
  name or user name, from the cache's voice states (the bot is left out).
- Stage channels: after joining one, the bot tries to become a speaker
  (`PATCH /guilds/{id}/voice-states/@me` with `suppress: false`, which needs Mute Members);
  if Discord refuses, it asks to speak (`request_to_speak_timestamp`). Its own voice state's
  `suppress` flag then keeps `VoiceStatus.stage` up to date: `Speaker`, `Requested` or
  `Audience` (moved back by a moderator, or the request failed, with a notice).
- **Leave this server**: asks first, leaves the channel if the bot is in one there, then leaves
  the server; the server's rules and the last channel (if there) are dropped from
  `discord.json`.
- Tab: **After logging in** and **Follow** pickers for the picked server; someone the bot
  follows who isn't in voice shows as "User <id> (not in a voice channel now)". People moving
  between channels only refresh the Follow list, so a pick in the server or channel list isn't
  reset (the channel pick is now remembered as it is made).
- Hand edits of `listen` and `servers` in `discord.json` apply live, like the other settings.

### D8 Packaging, CI, documentation

| Step (19 §4 D8) | State |
|---|---|
| 1. CI: ALSA headers, CMake | Done in D4. The D0–D4 commit's CI run passed on Windows, Ubuntu and macOS (lint, tests, release build, installer). |
| 2. `.deb`/`.rpm` | `.rpm` requires `alsa-lib` (D4); the `.deb` gets `libasound2` through `depends = "$auto"`. Not checked on a package built in CI (that only runs on a release tag). |
| 3. Windows installer | Nothing new needed: `dumpbin /dependents rtspcam.exe` lists only Windows system DLLs (no `opus.dll`, no C runtime DLL), so libopus and the C runtime are static. |
| 4. macOS `NSMicrophoneUsageDescription` | Still waits for an app bundle (11 phase M1). macOS stays "builds and passes its tests". |
| 5. README, progress document, index | This document, README's feature list and status table, the manual page, 00, 19's status line. |

Other:

- The tab scrolls (it is taller than the window's minimum size), only vertically: the content
  takes the width beside the scroll bar. Checked at 800×520.
- New screenshot: `screenshots/discord-tab.png`.
- `examples/bot_check.rs`: logs in with `RTSPCAM_DISCORD_TOKEN` from the environment (never a
  file or an argument), prints servers, channels and people in voice, and can join, unmute,
  listen, follow and watch: the tool used for §1.

## 4. Decisions beyond the plan

- **One channel at a time** stays (19 §7). The per-server rules decide which channel at login;
  following wins because it reflects where a person is now.
- **Follow picks from the people in voice**; a user id that isn't in voice can be put in
  `discord.json` by hand (the manual shows how). No text field for ids in the tab.
- **A sending track that fails rejoins** instead of only logging, so the problem in §2 would
  have shown in the tab.
- **The listen queue is the same lag-limited queue** (200 ms) as the send side.

## 5. Tests run

- `cargo test --workspace --locked`: all pass on Windows. New: 9 in `rtspcam-discord`'s
  session (listening and its output, a failed output, follow in/move/out, follow someone
  already in voice, login order follow > auto-join > last channel, a missing auto-join channel,
  stage request/speaker/audience, a voice channel has no stage state, a refused stage request,
  leaving a server) plus `songbird_can_play_the_queue` and the listen mix; 4 for `Mix` in
  `rtspcam-audio`; 2 for the new `discord.json` fields in `rtspcam-core`.
- `cargo clippy --workspace --all-targets --locked -D warnings` and `cargo fmt`: clean.
- Against Discord: §1.

## 6. Not verified (needs a person)

- **Hearing the bot**: sound quality and latency by ear, mute and unmute as heard, the gate.
  The bot's track now plays, but nobody has listened to it.
- **Hearing the channel**: someone speaking while the bot listens ("N people talking", and
  the sound on the output).
- **Following** someone as they move between channels and leave (only "already in voice at
  login" was seen).
- **Stage channels**: the test server has none.
- **Leave this server**: not tried (it would remove the bot from the only test server).
- Rejoining after pulling the network, being moved or kicked by a person.
- Linux and macOS at run time; the `.deb` dependency list from a real package build.
