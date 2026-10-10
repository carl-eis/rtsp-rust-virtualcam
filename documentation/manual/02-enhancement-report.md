# 02 — Enhancement report: what to build next, and what camera audio would cost

Date: 2026-10-10. Branch `feat/performance` at `664c914` (v1.1.0), **updated for `1d5d508`**:
master's phase L (Linux virtual cameras through v4l2loopback, `.deb`/`.rpm`,
[13](../13-progress-phase-l.md)). Written from the code and the documents in `documentation/`. Nothing was changed or run apart from reading sources. Effort figures are
working days for one developer who already knows this codebase. They cover building, testing and
documenting. They do not cover the checks that need a person.

## 1. Summary

- **The product is close to useful, but the main claim is not yet checked.** Nobody has seen a
  camera from this app in Discord, Teams, OBS or Chrome, and the installer has not been run on a
  clean PC. These are the cheapest and most important next steps (§3, P0).
- **Linux now has virtual cameras (phase L), but no browser or call app has been tried.** The
  v4l2loopback backend was checked only with ffmpeg and v4l2-ctl, on WSL2's kernel. Checking
  Chrome, Firefox, Zoom and Discord on a real desktop, plus the 0.12.7 module that Ubuntu
  22.04/24.04 and Debian 12 ship, joins the Windows checks at the top of the list. macOS
  (phase M) is the remaining camera gap.
- **There are several small, cheap improvements in the code** (§2). The most important: *any*
  edit to a stream, including rotating the picture, removes and re-creates the virtual camera,
  so an app in a call loses it.
- **Audio as a microphone:** getting audio from the camera and decoding it is moderate work, about
  1.5–2.5 weeks. The cost depends on the **virtual microphone**, which differs by OS:
  - **Linux:** easy (PipeWire, about 1–1.5 weeks). Now that Linux ships packages, this is the
    natural first native microphone.
  - **Windows:** a real microphone of our own needs a **signed kernel-mode audio driver**.
    That means months of work, an EV certificate and attestation signing. There is no user-mode
    API, and the MF virtual camera carries no audio.
  - **macOS:** needs a Core Audio HAL plug-in (about 4–6 weeks).
  - **Recommended first step:** send the audio to an output device the user picks. With a free
    virtual cable installed (VB-Cable on Windows, BlackHole on macOS, a null sink on Linux),
    that device's other end works as a microphone. On all three OSes this totals **about 3–4
    weeks**, and it also gives "listen" in the preview at no extra cost. Details in §4.

## 2. Findings in the code

These come from reading the code, not from the existing documents. Each one names its file.

### 2.1 Any stream edit tears down the virtual camera

`reconcile()` (`crates/rtspcam-engine/src/manager.rs`) keeps a camera only if its whole
`StreamConfig` is unchanged (`**s == cam.config`). Changing the rotation, the overlay text, the
fit mode, the on-demand flag or the password has the same effect: the RTSP pipeline stops, the
MF camera is removed (`backend.remove`) and created again. Discord or Teams in a call loses the
device and usually does not switch back by itself.

Split the changes into three levels:

| Change | Today | Should be |
|---|---|---|
| `picture`, `fit_mode` | camera + pipeline rebuilt | applied live: make `CameraShared::picture`/`fit` swappable (`RwLock` or `ArcSwap`) and clear the `Adjusted` cache |
| host, port, path, credentials, transport, `on_demand` | camera + pipeline rebuilt | restart the pipeline/supervisor only; keep the camera |
| name, `output` (preferred format) | camera + pipeline rebuilt | camera re-created (the friendly name and format list are fixed at creation) |

**Effort:** 1.5–2 days, with tests in `crates/rtspcam-engine/tests/manager.rs`. **Value: high.**
Users will tune the picture while a call is running, so this is when it matters most.

**On Linux it is worse.** Removing a camera while an app reads it fails with `EBUSY`
(`remove_device` in `crates/rtspcam-platform/src/linux/camera.rs`), so the device stays behind
until later. With module 0.12 there are four fixed devices, "RTSP Cam 1–4", that streams take in
turn. A rebuilt stream can therefore come back on a *different* device, and the app in the call
is left reading a device nobody writes to.

### 2.2 Fixed output formats, defined in two places

- The DLL advertises 1280×720, 1920×1080 and 640×480 at 30 and 15 fps
  (`crates/rtspcam-vcam/src/formats.rs` `SIZES`/`RATES`). Core validates against its own copy
  (`OutputFormat::RESOLUTIONS`/`FRAME_RATES` in `crates/rtspcam-core/src/config/mod.rs`).
  The DLL already depends on `rtspcam-core`, so it should use core's constants. Otherwise one
  side can change without the other.
- No portrait formats. Rotating a 16:9 camera by 90° (an option the app offers) gives a 9:16
  picture that is letterboxed into 16:9, so most of the frame is black bars. Offering
  720×1280 and 1080×1920, plus 960×720 (4:3) and 60 fps where the source has it, is cheap.
  Apps pick from the list, and the preferred format is moved to the front.
- **On Linux this matters more.** A v4l2loopback device has exactly one format: YUYV at the
  stream's configured `output` (`create` in `linux/camera.rs`). On Windows, apps choose from a
  list; on Linux, the setting in the stream dialog is all they get. A portrait or 4:3 choice, or
  "match the source", is the only way to avoid the black bars there.
- **Effort:** 1–2 days. Check that Discord and Teams accept portrait media types; this needs a
  person.

### 2.3 Allocation for every frame inside Frame Server

`send_sample` (`crates/rtspcam-vcam/src/stream.rs`) calls `MFCreateMemoryBuffer` + `MFCreateSample`
for every frame, and `feed.rs` copies every received frame into a new `Vec` (`data.to_vec()`). At
1080p RGB32 and 30 fps, that is about 250 MB/s of allocation churn **inside the Frame Server service**.
A small pool fixes it: reuse 3–4 buffers, or use `MFCreateVideoSampleAllocatorEx`, and read the
pipe straight into a pooled buffer. This belongs in step 8.0/8.3 of
[08.1](../08.1-Plan-Phase-8.md); measure it there. The ~9 MB/h memory drift seen in the Phase 2
soak test might be related, but that is a guess and should be checked under the same
measurement.
**Effort:** 1–2 days.

### 2.4 One scaler shared by every consumer of a camera

`CameraSource` (`crates/rtspcam-engine/src/source.rs`) keeps one `Scaler` and a seq behind one
`Mutex`. Two apps reading the same camera in different formats (OBS at 1080p NV12, Teams at 720p)
take turns on the lock, and the scaler's plan is rebuilt on every switch. The non-NV12 paths
also allocate a scaled intermediate `Frame` per call (`st.scaler.scale(..)`). 08.1 §8.3 already
plans "cache per (frame, format)". This is the concrete reason for it. **Effort:** 1 day.
This applies to Windows only: on Linux each camera has one `Pusher` reading one format, so there
is no contention.

### 2.5 Test connection opens a second RTSP session

`probe::test_connection` always starts a new `Pipeline`. For a stream that is already running,
this opens a second session, which single-session cameras reject (the known
`EOF while expecting response to PLAY`, [06 §5](../06-checkpoint-2026-10-09.md)). When the
dialog edits an existing, running stream and the address and credentials are unchanged, report
from the running pipeline instead: its `StreamState` and `FrameBus::latest()`.
**Effort:** 0.5–1 day.

### 2.6 Status data that is collected but not shown

`StreamStats` already counts `packets_lost`, `decode_errors` and `frames_decoded`. The info panel
(`crates/rtspcam-app/src/ui/app.rs`, `update_info`) shows codec, size, fps, bitrate, latency and
decoder, but not loss or errors. It also does not show how long the stream has been up or how
many times it reconnected. Packet loss is the first thing to check when a Wi-Fi camera stutters.
**Effort:** 0.5 day.

### 2.7 No "retry now"

After repeated failures, the backoff waits up to 30 s (`BackoffConfig::max`), and permanent
errors such as a wrong password always wait the maximum. After fixing the camera, the user has
to wait or toggle the stream. Add a "Reconnect now" action that restarts the supervisor's
pipeline. The status could also show the countdown (`Retrying { retry_at }` is already there).
**Effort:** 0.5 day.

### 2.8 Colour handling

`Matrix::for_height` picks BT.601 or BT.709 from the picture height, and full-range sources
(most MJPEG, some H.264 cameras) are treated as limited range
([03 §6](../03-progress-phases-2-3.md)). As a result, skin tones and blacks can look slightly
wrong. The H.264/H.265 SPS VUI carries `video_full_range_flag` and `matrix_coefficients`. retina
exposes the SPS, so the matrix and range can travel on `Frame`. Note that NV12 to the DLL is
passed through unchanged, so for NV12 consumers a range mismatch must be fixed by expanding the
range or by setting `MF_MT_VIDEO_NOMINAL_RANGE` on the media type. **Effort:** 2–3 days.

## 3. Recommended backlog

Ordered by value for effort. "P" is priority.

### P0 — confirm the product works (needs a person, about 1–2 days)

1. Cameras in Discord (including the **remote** side of a call), Teams, Zoom, OBS, Chrome and
   the Camera app. Record the results in a compatibility table (planned in 08.1 §7).
2. Run the installer on a clean Windows 11 VM: install, use, uninstall.
3. **Linux on a real desktop** ([13 §5](../13-progress-phase-l.md)):
   - The `.deb` on Ubuntu 24.04 (module 0.12.7, GNOME on Wayland) and on Debian 13 or
     Ubuntu 26.04 (0.13+).
   - Cameras in Chrome, Firefox, Zoom, Discord and OBS. Above all, check that Chrome lists the
     device and accepts YUYV.
   - Secure Boot (MOK) enrolment, and the udev rule that lets the logged-in user add cameras.
   - Reading frames on 0.12.7. This was never seen working: the test crashed the WSL VM with a
     forward-ported module.
4. Look at the latest CI run, including the new `linux-packages` job.

If a major app rejects the camera, everything below waits. On Linux, this applies to a failure
on 0.12: that is the version most users have today.

### P1 — high value, small (about 1 week in total)

| Item | Effort | Why |
|---|---|---|
| Live picture/fit changes; pipeline-only restarts (§2.1) | 1.5–2 d | Stops calls from losing the camera |
| Reconnect now + loss/errors/uptime in the info panel (§2.6, §2.7) | 1 d | The first two support questions |
| Test connection reuses the running session (§2.5) | 0.5–1 d | Fixes a known failure |
| **Help > Save diagnostics…**: a zip with app logs, `vcam.log` from `%ProgramData%`, the config with passwords removed (`config show` already redacts), the OS version, the decoder list and the camera statuses | 1–1.5 d | Without it, every bug report means asking for 4 files by hand |
| Per-camera on/off in the tray menu (only "Pause all" exists today) | 1 d | Privacy: turn one camera off without opening the window |
| Duplicate stream (main and sub stream of the same camera, or a second crop) | 0.5 d | Common setup; today all fields are typed again |
| Save snapshot (PNG of the current, adjusted frame; the preview already makes RGBA) | 0.5–1 d | Cheap, and often requested |
| **Linux: handle SIGTERM like Ctrl+C.** The GUI app ignores SIGTERM, which it gets at logout and shutdown, so its devices stay behind until the next start ([13 §5](../13-progress-phase-l.md)). `main.rs` only waits on `ctrl_c()`, and only in headless mode | 0.5 d | Needs a small hook in `rtspcam-platform`, so it passes the cfg check |
| **Linux: show which device a stream got** ("Camera: RTSP Cam 2, /dev/video3") in the info panel. `create` returns only `Ok(())`, so the UI can't say which device | 0.5–1 d | On 0.12 the device names don't follow the streams, so users can't tell which "RTSP Cam N" to pick in an app. Needs `create` to return a device description (a small `VirtualCameraBackend` change; Windows returns the camera name) |

### P2 — the large gaps (planned already)

| Item | Effort | Notes |
|---|---|---|
| ~~Phase L: v4l2loopback backend~~ | done in `a96f1e1` ([13](../13-progress-phase-l.md)) | What is left is the P0 Linux checks |
| Phase M: macOS camera extension | ~3–5 weeks | Now the only OS without cameras. Swift extension, signing, notarization; needs an Apple developer account. The app itself has never been started on a Mac, so run it there first (1 day) |
| Phase 8.0 measurement, then 8.1/8.2 | 8.0: 2–3 d; 8.1+8.1a: 1–1.5 weeks; 8.2: 1 week | Follow [08.1](../08.1-Plan-Phase-8.md) as written. Add §2.3 and §2.4 to the 8.0 measurements |
| Code signing for the release (Azure Artifact Signing / an OV certificate) | 1 d + cost | The release workflow already accepts a certificate. Unsigned installers get SmartScreen warnings |

### P3 — medium features

| Item | Effort | Notes |
|---|---|---|
| More output formats: portrait, 4:3, 60 fps (§2.2) | 1–2 d | Makes rotation usable. **Moved up** (see §5): on Linux this setting is the only format apps get |
| Use the sub stream for small consumers: an optional second URL per stream, used for the preview and for consumers asking ≤ 640×480 | 4–6 d | Big CPU saving for 4K cameras. ONVIF discovery already lists both streams. Fits 08.1 §8.2. Switching streams mid-call needs care (a placeholder frame, or keep both running for the grace period) |
| Colour range and matrix from the VUI (§2.8) | 2–3 d | |
| Update check (GitHub releases, opt-in, with a link in About) | 1 d | No self-update; just "version X is available" |
| Notifications when a camera goes offline | 2–3 d | Slint's tray has no balloons (the reason they were removed in [10 §2](../10-progress-cross-platform.md)). On Windows this needs toasts with an AppUserModelID, which the installer's Start-menu shortcut can supply |
| Basic UI tests with Slint's testing backend | 2–3 d | No UI tests exist; a paint-handler crash once exited with no log ([06 §5](../06-checkpoint-2026-10-09.md)) |
| Fuzzing the IPC parser and config loader (08.1 §7) | 2 d | The DLL parses input inside a system service |

### P4 — larger, optional

| Item | Effort | Notes |
|---|---|---|
| Camera audio as a microphone | see §4 | |
| ONVIF PTZ: a direction pad in the preview, plus presets | ~1 week | The ONVIF client already speaks SOAP and handles authentication and clock skew |
| PTZ through the **webcam's own controls** | +1–2 weeks after PTZ | The DLL already implements `IKsControl` (it returns "not found"). Answering `PROPSETID_VIDCAP_CAMERACONTROL` pan/tilt/zoom would let the camera-settings panels in Teams and Windows move the IP camera. That would be unusual for this kind of app |
| RTSPS | 1–2 weeks | retina accepts only `rtsp://` (`retina-0.4.21/src/client/mod.rs:1182`) and opens its own TCP connection, so TLS needs an upstream retina change or a fork. Check with the retina maintainer first |
| HTTP MJPEG / snapshot-polling sources (plan §5) | 3–5 d | Covers cheap cameras and some NVRs. Windows already decodes MJPEG; elsewhere it needs a JPEG decoder (`zune-jpeg`) |
| Windows 10 (DirectShow filter) | 3–5 weeks | Windows 10 support ended in October 2025, so this is lower priority than when the plan was written |

Not recommended: recording/NVR features (a different product; the plan excludes them), importing
from Blue Iris or Synology (proprietary formats, very few users), and stream "groups" before
anyone has more than about 8 cameras.

## 4. Audio from the camera as a microphone

### 4.1 What exists today

- `RtspSource::connect_inner` runs `SETUP` on **only the video stream**, and `next_frame` skips
  every item that isn't `CodecItem::VideoFrame` (`crates/rtspcam-pipeline/src/source.rs`). Audio
  is never requested from the camera.
- retina 0.4.21 depacketizes these audio encodings (`retina/src/codec/mod.rs:852-881`):
  **AAC** (`mpeg4-generic`), **G.711** (`pcma`, `pcmu`), **G.722**, **G.726**-16/24/32/40,
  **DVI4**, **L16**, `u8` and **G.723**. Not supported: Opus, MP2/MPA (some Hikvision models offer
  "MP2L2") and G.729. Choosing any of these fails `SETUP` for that track. That is fine if audio
  is optional.
- What cameras usually send: G.711 µ-law/A-law 8 kHz mono (most common), AAC-LC 8–48 kHz
  (newer models), sometimes G.726. Many cameras have audio **off** by default, or have no
  microphone at all.
- ONVIF profiles can carry an `AudioEncoderConfiguration`. `query_camera` currently skips
  audio-only profiles, but it could report "has audio" for the Find cameras dialog.
- Nothing downstream handles audio. `Frame`, `FrameBus`, `FrameSource`, the IPC protocol and the
  DLL are video-only. **They do not need to change** for the recommended design (§4.4): audio
  never goes to the camera DLL.

### 4.2 What it takes, layer by layer

**Ingest (pipeline):**
- An opt-in per stream (`audio.enabled`, default **off**). Setting up a second track changes the
  RTSP session (another interleaved channel, or a second UDP port pair), and some cameras
  misbehave when it is set up. Existing streams must not change behaviour.
- `pick_audio_stream` next to `pick_video_stream`: run `setup` on both before `play`. Turn
  `RtspSource::next_frame` into `next_item` → `Video(EncodedFrame) | Audio(EncodedAudio)`.
- Audio must not wait behind video decoding. Decode it on the ingest task (G.711/AAC decoding
  is microseconds per packet) or on its own small thread, not on the video decode thread's
  bounded channel.
- If the audio codec isn't supported, log it once, show "audio: G.729 not supported", and keep
  the video working.

**Decode:**
- G.711 (µ/A-law): a 256-entry table, about 30 lines. L16: byte swap. G.726: about 300 lines,
  or a small crate. G.722: rare, skip at first.
- AAC-LC: `symphonia-codec-aac` (pure Rust, **MPL-2.0**, LC only, no HE-AAC). On Windows, the
  Microsoft AAC decoder MFT is another option. Keep it portable unless symphonia fails on real
  cameras.
- Output: f32 PCM at the source rate, with timestamps from the RTP clock.

**Buffering — this differs from video:**
- Video uses "latest frame wins" (`watch` channel). **Audio cannot drop samples like that**
  without audible clicks. It needs a per-consumer ring buffer with a jitter buffer
  (≈ 80–200 ms target fill).
- **Clock drift is certain.** The camera's sample clock and the PC's audio device clock differ by
  tens of ppm, and cheap cameras by more. Over a 1-hour call that adds up to tens to hundreds of
  ms, so without correction the buffer slowly fills up or runs dry. The standard fix is an
  adaptive resampler (the `rubato` crate) whose ratio is adjusted slightly to keep the
  ring buffer at its target fill. The same resampler also converts 8 kHz to the device's 48 kHz.
- On gaps (reconnect, packet loss): play silence and fade in and out over a few ms.

**Lip sync:**
- The video path adds decode (2–14 ms), at most one frame of pacing in the DLL, and whatever the
  consuming app adds. The audio path adds the jitter buffer (≈ 100+ ms). Audio will lag video
  slightly, and the camera's own A/V offset varies by model anyway. A per-stream **audio delay**
  setting (−200…+500 ms, applied as extra buffering) is enough. Apps treat the microphone and the
  camera as unrelated devices, so exact sync is not possible.

**On-demand behaviour:**
- Today, an on-demand stream connects only while the *camera* has a consumer. A microphone
  consumer is not visible on Windows or macOS when we render into a third-party cable. Simplest
  rule: **audio follows the camera.** It flows while the camera or the preview is in use. Using
  the microphone without the camera then needs the stream set to "always connected". PipeWire
  can report when the source is in use, so on Linux the microphone can count as demand too.
  With v4l2loopback 0.13+, the camera already reports its readers (13 §1), so on Linux the camera
  and the microphone can each start the stream. On 0.12 there is no reader information, and
  on-demand streams already run all the time there.

**Config, UI and status:**
- `audio: { enabled, device, delay_ms, volume }` on `StreamConfig` (with `serde(default)`, so no
  migration is needed).
- The info line shows the audio codec and rate, and "audio: not offered by the camera" when
  that applies. Add a "Listen" toggle in the preview.
- Add audio to the test server: mediamtx + ffmpeg with `-c:a pcm_mulaw` and `-c:a aac` tracks
  (`tools/test-rtsp/`). Without this, nothing can be tested in CI.

### 4.3 The virtual microphone, per OS

| OS | Own virtual mic | Using an existing virtual device |
|---|---|---|
| **Windows** | Needs a **kernel-mode audio driver** (PortCls/WaveRT based on Microsoft's SysVAD sample, or the newer ACX), with one capture endpoint per camera. The app feeds it through an IOCTL or a shared ring buffer. Windows has **no user-mode API** that creates a capture endpoint: WASAPI and `IAudioRenderClient` only use endpoints that already exist. The MF virtual camera (`MFCreateVirtualCamera`) is a video capture source and carries no audio. Requirements: an EV code-signing certificate and **attestation signing through Microsoft Partner Center** for each driver build; installer changes (`pnputil`, device creation); a kernel bug means a blue screen. Writing it in Rust (`windows-drivers-rs`) is possible but still early. **Effort: 6–12 weeks** for someone with driver experience, plus certificate costs and ongoing maintenance. A commercial alternative is to license a virtual audio driver SDK (e.g. Thesycon TVirtAudio), or VB-Audio's OEM licence, and bundle it | Render with WASAPI shared mode (via `cpal`) to an output device the user chooses, e.g. **"CABLE Input (VB-Audio Virtual Cable)"**. Apps then pick "CABLE Output" as the microphone. VB-Cable is free for users to install, but **may not be bundled** without a licence; one cable means one microphone (VB-Audio sells extra cables). Other options: Voicemeeter, or any virtual cable the user already has. This is also how other virtual-camera projects handle audio. **Effort: 3–5 days** on top of §4.2 |
| **Linux** | **PipeWire**: create a node per camera with `media.class = Audio/Source/Virtual` and a name like "Front Door (RTSP Cam)" through `pipewire-rs`. PulseAudio-based apps (Discord, Chrome, Zoom) see it through `pipewire-pulse`. It runs entirely in user space with no root, and node state shows when it is in use. Fallback for PulseAudio-only systems: `module-null-sink` + `module-remap-source` (via `pactl`) and render into the sink. Build note: `pipewire-rs` links `libpipewire-0.3`, which needs a `-dev` package to build and a runtime dependency in the `.deb`. **Effort: 5–8 days** | Same `cpal` path into a null sink the user made |
| **macOS** | A **Core Audio AudioServerPlugIn** (HAL plug-in) bundle in `/Library/Audio/Plug-Ins/HAL`, loaded by `coreaudiod`. It is written in C, C++ or Swift against a C API (Rust is possible but unusual). Getting audio from the app into `coreaudiod`'s sandbox goes through shared memory or XPC. It needs an admin install and a `coreaudiod` restart, plus signing and notarization. BlackHole is the well-known open-source example, but it is GPL-3.0, so use it only as a reference unless the project's licence allows it. (AudioDriverKit is the other route, but it targets hardware-backed drivers.) **Effort: 4–6 weeks** | Same `cpal` path into **BlackHole** (free, user-installed) |

### 4.4 Recommendation and total effort

Build it in stages. Each stage can ship on its own:

| Stage | What | Effort | OS |
|---|---|---|---|
| A | Audio ingest (opt-in), G.711/L16/AAC decoding, test-server audio tracks, CLI `probe` shows the audio track | 4–6 d | all |
| B | Audio routing core: per-consumer ring buffer, jitter buffer, drift-correcting resampler, silence on gaps, delay/volume, config fields, status text | 4–6 d | all |
| C | Output to a chosen device (`cpal`) + device picker in the stream dialog + "Listen" in the preview. With VB-Cable or BlackHole installed, this gives a working microphone | 3–5 d | all |
| | **Subtotal: usable microphone on all three OSes with a free third-party cable** | **≈ 2.5–3.5 weeks** | |
| D | Native PipeWire source per camera (no cable needed), microphone use counts as on-demand demand | 5–8 d | Linux |
| E | Native macOS HAL plug-in | 4–6 weeks | macOS |
| F | Own signed Windows driver, **or** a licensed driver SDK | 6–12 weeks (or a licence fee + 1–2 weeks integration) | Windows |

Recommendation: build **A–C**, release, and find out whether users actually ask for audio.
Many people use RTSP Cam for a room or doorbell view where the camera's microphone is either
not wanted or poor (8 kHz G.711).

With phase L shipped, **D is now the cheapest native microphone on any OS.** It reuses the same
idea as the cameras (one device per stream, named after it), and the `.deb`/`.rpm` already
exist. It adds a dependency, though: `libpipewire-0.3` at build time (for `pipewire-rs`) and
in the packages. To avoid that, drive PulseAudio modules through `pactl` instead, which runs on
PipeWire through `pipewire-pulse`.

A reasonable order is A → B → D, so Linux gets a working native microphone first, then C for
Windows and macOS through a third-party cable.
Treat **F** as a business decision (driver signing, liability for blue screens, ongoing
maintenance), not as engineering work to start now.

**Risks:**
- Some cameras break when the audio track is set up. Mitigation: audio is opt-in per stream,
  and if audio setup fails, the stream retries once with video only.
- The AAC decoder doesn't handle a camera's profile (HE-AAC). Mitigation: on Windows, fall back
  to the MF AAC decoder.
- Users don't understand the virtual-cable step. Mitigation: the device picker detects known
  cables by name and links to an install guide when none is found.
- Echo or feedback when "Listen" plays through speakers while the camera's microphone hears
  them. Mitigation: "Listen" is off by default; show a warning.

## 5. Suggested order

1. P0 checks with real apps on Windows **and Linux** (a person, about 3–4 days).
2. P1 bundle (about 1.5 weeks), starting with §2.1, which phase L makes more urgent, plus the two
   Linux items.
3. Output formats (§2.2), moved up from P3: on Linux they decide what every app gets.
4. 08.1 step 8.0 measurement, including §2.3 and §2.4; then 8.1, 8.1a and 8.2 as the numbers
   say. 8.0 can now be measured on Linux end to end too, since frames reach a real device.
5. If audio is wanted: stages A → B → D (Linux native microphone), then C.
6. Phase M, P3 items as needed, and P4 when users ask for them.

## 6. Notes on confidence

- Facts about the code come from reading it on this branch. Line references are to
  `664c914`.
- Facts about retina come from the vendored source `retina-0.4.21`.
- "No user-mode virtual audio endpoint on Windows" and "the MF virtual camera is video-only"
  match Microsoft's documentation and how other virtual-camera projects work around it. A
  1-hour spike (`MFCreateVirtualCamera` with an audio stream descriptor) would confirm the second
  point for good.
- Effort figures are estimates, not measurements. The Windows driver and macOS plug-in figures
  are the least certain.
