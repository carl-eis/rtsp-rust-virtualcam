# RTSP Cam — Enhancement & QOL Report

> Prepared: 2026-10-10
> Based on: workspace root `C:\Users\carle\.herdr\worktrees\rtsp-rust\feat-performance`, branch `feat-performance`
> Project status: Phases 1–7 built and partially verified; Linux/macOS virtual camera groundwork (Phase G) complete.

---

## 1. Project Overview

**RTSP Cam** turns RTSP streams from IP cameras, NVRs, or `mediamtx` into virtual webcams on the local machine. Each stream appears as its own camera in Discord, Teams, Zoom, OBS, Chrome/Edge and the Windows Camera app.

### Current Architecture (8 crates)

| Crate | Role | Key deps |
|---|---|---|
| `rtspcam-core` | JSON config model, protocol/URL builder, encrypted secrets, validation, atomic save, file watching, logging | serde_json, DPAPI/SecretService/Keychain |
| `rtspcam-ipc` | Wire protocol between app and vcam DLL, `FrameSource` trait, placeholder pictures | — |
| `rtspcam-pipeline` | RTSP ingest (retina), H.264/H.265/MJPEG decoding (OpenH264 + platform MFTs), NV12 scaling, FrameBus, reconnects | retina, openh264, tokio |
| `rtspcam-onvif` | WS-Discovery scan + GetProfiles/GetStreamUri (SOAP) for camera auto-discovery | reqwest, quick-xml |
| `rtspcam-platform` | OS boundary: `VirtualCameraBackend`, `SecretStore`, `Autostart`, `SingleInstance`, `FrameTransport`, `PlatformDecoders` | `windows`, `nix`, `objc2` (macOS) |
| `rtspcam-engine` | `CameraManager`, preview, status, stream form, overlay (name/time text), `CameraSource` | all above |
| `rtspcam-app` | Slint desktop UI: main window, stream table, preview, dialogs (add/edit/discover/picture/settings), tray icon | slint (~1.18), rfd, tokio |
| `rtspcam-cli` | Developer CLI: probe, view, onvif, `vcam` commands | — |
| `rtspcam-vcam` | Media Foundation custom media source DLL (Windows only, runs inside Frame Server) | windows, rtspcam-ipc |
| `rtspcam-vcam-mgr` | Safe wrapper over `MFCreateVirtualCamera` (Windows only) | windows |

### Current Feature Matrix

| Feature | Windows 11 | Linux | macOS |
|---|---|---|---|
| UI, preview, dialogs, tray | ✅ | ✅ | ✅ (untested) |
| Virtual cameras | ✅ MF virtual camera | ❌ not yet (v4l2loopback planned) | ❌ not yet (CoreMediaIO planned) |
| Decoding | ✅ H.264, H.265, MJPEG | ✅ H.264 (OpenH264) | ✅ H.264 (OpenH264) |
| Audio | ❌ never implemented | ❌ | ❌ |
| Passwords | ✅ DPAPI | ✅ SecretService / keyfile | ✅ Keychain / keyfile |
| Start at login | ✅ HKCU Run key | ✅ XDG autostart | ✅ LaunchAgent |

### What Works Today (on Windows 11)

- App opens, lists streams, shows live preview with codec/size/fps/bitrate/latency info
- Add/Edit dialog with validation, pasted URLs, Test Connection; Settings; tray icon; minimize-to-tray
- Single-instance guard, Close/Quit ends the process in ~40 ms
- Each enabled stream becomes a Windows virtual camera (verified through Frame Server with test pattern, 30 fps, 12 formats)
- Hard kill of the process removes cameras
- ONVIF camera discovery
- Brand presets (Hikvision, Dahua, Amcrest, Reolink, Tapo, UniFi Protect, Axis, Foscam)
- Picture options: rotate, flip, crop, name/time overlay, freeze-last-frame on disconnect
- Import/export streams (passwords excluded)

### What Needs Person-Only Verification

- Cameras in Discord (preview **and** remote side), Chrome, OBS, Windows Camera app
- Phase 3 one-hour soak test (memory leak check)
- Phase 7 installer on a clean machine
- Linux/macOS functionality (no backend yet)

---

## 2. QOL & Enhancement Recommendations (Ranked by Effort)

### Tier 1 — Small, High-Value (1–3 days each)

#### 2.1 Stream Health Dashboard / Metrics Page
**Effort:** ~2 days
**Impact:** High — users want to know "is my camera working right?"

Currently the status bar shows only camera count and error count. The pipeline already computes fps, bitrate, latency, frames decoded, decode errors, and packets lost per stream. Expose these in the UI.

**What to do:**
- Add a "Health" column to the stream table showing a compact status (e.g., "14.9 fps, 12 kb/s, 7 ms")
- Clicking a row expands a health panel (like the existing info label next to the preview)
- Add a "Stats" button in the preview that cycles through: fps, bitrate, latency, packet loss, frames decoded
- Use the existing `StreamStats` from `rtspcam_pipeline::status` — no new data needed, just expose it

**Files to touch:**
- `crates/rtspcam-engine/src/status.rs` — add health summary string method
- `crates/rtspcam-app/ui/app.slint` — add health column/panel
- `crates/rtspcam-app/src/ui/app.rs` — wire up the display

#### 2.2 Dark Mode / Theme Support
**Effort:** ~2 days
**Impact:** Medium — Slint has built-in theme support

Slint supports `Palette` customization. Add a theme setting to `AppSettings` and let the UI respond.

**What to do:**
- Add `theme: "light" | "dark"` to `AppSettings` in `rtspcam-core/src/config/mod.rs`
- Set the Slint palette based on the setting in `app.rs`
- Slint's `backend-winit` renderer supports dark mode natively

**Files to touch:**
- `crates/rtspcam-core/src/config/mod.rs` — add `theme` field
- `crates/rtspcam-app/ui/dialogs.slint` — add theme toggle to settings
- `crates/rtspcam-app/src/ui/settings_dialog.rs` — persist theme
- `crates/rtspcam-app/src/ui/app.rs` — apply palette on startup and on change

#### 2.3 Keyboard Shortcuts
**Effort:** ~1 day
**Impact:** Medium — power users will love this

Slint supports global and window-level shortcuts via the `key` property and `onKeyPressed` handlers.

**What to do:**
- Ctrl+N: new stream (open Add dialog)
- Ctrl+E: edit selected stream
- Ctrl+D: remove selected stream
- Ctrl+R: toggle selected stream
- F5: refresh / retest connection
- Ctrl+Shift+S: open settings

**Files to touch:**
- `crates/rtspcam-app/ui/app.slint` — add `Shortcut` elements and `onKeyPressed`
- `crates/rtspcam-app/src/ui/app.rs` — bind callbacks

#### 2.4 Config File Backup Retention
**Effort:** ~1 day
**Impact:** Low-medium

Currently one `.bak` file is kept. Keep a rolling set (e.g., last 3) for safety.

**Files to touch:**
- `crates/rtspcam-core/src/config/store.rs` — modify the save logic to keep N backups

---

### Tier 2 — Medium Effort (3–7 days each)

#### 2.5 Batch Operations
**Effort:** ~3 days
**Impact:** Medium — users with 5+ cameras need this

- Batch enable/disable all streams
- Batch change output resolution/FPS
- Batch move streams to a different folder (Windows) / group

**What to do:**
- Multi-select support in the Slint `StandardTableView` (hold Ctrl/Cmd + click)
- A "Batch" menu with common operations
- Or simply: right-click context menu with "Enable all" / "Disable all" / "Set resolution to 720p for all"

**Files to touch:**
- `crates/rtspcam-app/ui/app.slint` — add multi-select
- `crates/rtspcam-engine/src/manager.rs` — add batch methods

#### 2.6 Stream Groups / Tags
**Effort:** ~3 days
**Impact:** Medium — useful for organizing many cameras

Let users group streams (e.g., "Indoor", "Outdoor", "Office"). Filter the table by group.

**What to do:**
- Add `groups: Vec<String>` to `StreamConfig`
- Add a group selector in the Edit dialog
- Add a filter dropdown in the main window

**Files to touch:**
- `crates/rtspcam-core/src/config/mod.rs` — add `groups`
- `crates/rtspcam-app/ui/` — add group UI
- `crates/rtspcam-engine/src/manager.rs` — filter by group

#### 2.7 Screenshot / Snapshot Export
**Effort:** ~3 days
**Impact:** Medium — users want to capture a still from the stream

**What to do:**
- Add a "Capture" button in the preview pane
- Export the current frame as a PNG/JPEG to the user's Pictures folder
- Use the existing `Frame` data (BGRA conversion already exists in `CameraSource::next_frame` for RGB32)

**Files to touch:**
- `crates/rtspcam-app/ui/app.slint` — add capture button
- `crates/rtspcam-app/src/ui/preview.rs` — trigger capture
- New file: `crates/rtspcam-app/src/ui/snapshot.rs` — encode BGRA to PNG

#### 2.8 RTSPS (RTSP over TLS)
**Effort:** ~5 days
**Impact:** Medium — growing number of cameras require TLS

The `retina` crate supports SRTP but the plan (§5) lists RTSPS as "Not in scope." However, many modern cameras only offer RTSPS. This is a config-level change.

**What to do:**
- Add `rtsps` as a protocol variant in `Protocol`
- In `RtspSource::connect_inner`, wrap the TCP stream with Rustls TLS before handing it to `retina`
- Handle certificate verification (option to skip for self-signed, with a clear warning)

**Files to touch:**
- `crates/rtspcam-core/src/config/protocol.rs` — add `RtspS` variant
- `crates/rtspcam-pipeline/src/source.rs` — TLS wrapper in connection
- `crates/rtspcam-app/ui/` — protocol selector update

**Dependencies:** `rustls`, `rustls-pemfile`

#### 2.9 Import from Existing Configs
**Effort:** ~3 days
**Impact:** Medium — users migrating from other software

Support importing from:
- Synology Surveillance Station export
- BlueIris config
- Generic RTSP URL list (one per line)
- `mediamtx` config

**Files to touch:**
- `crates/rtspcam-core/src/config/transfer.rs` — add new import formats
- `crates/rtspcam-app/src/ui/files.rs` — add import dialog options

---

### Tier 3 — Larger Efforts (1–2 weeks each)

#### 2.10 GPU-Accelerated Decoding (DXVA / Video Processor MFT)
**Effort:** ~1–2 weeks
**Impact:** High — current CPU decoding is fine for 1–2 streams but doesn't scale

Plan Phase 8 (from 01-plan.md) already calls for this. Currently `rtspcam-pipeline` uses OpenH264 (software H.264) as the default and Media Foundation MFTs as platform decoders, but both are software-mode. A GPU path would use:
- DXVA for H.264/H.265 decoding (hardware decode)
- Video Processor MFT for scaling (GPU scaling)
- DXGI surface samples into the virtual camera

**What to do:**
- Implement `MfD3dDecoder` in `rtspcam-pipeline/src/decode/` that creates DXVA decoder MFTs
- Modify `Scaler` to use the Video Processor MFT instead of CPU scaling
- Modify the vcam DLL to accept DXGI surfaces (current code uses memory buffers)

**Files to touch:**
- `crates/rtspcam-pipeline/src/decode/` — new `openh264_mf.rs` or `dxva.rs`
- `crates/rtspcam-pipeline/src/scale.rs` — add GPU scaling path
- `crates/rtspcam-vcam/src/` — support DXGI surface samples
- `crates/rtspcam-core/src/config/` — add `decoder: "auto" | "gpu" | "cpu"` setting

#### 2.11 Snapshot / Clip Recording
**Effort:** ~1 week
**Impact:** Medium — users want to record from cameras

**What to do:**
- Add a "Record" toggle per camera
- Record H.264 from the RTSP source directly (no decode needed, just remux)
- Or record decoded frames to MJPEG/MP4
- Save to a configurable folder, with a rolling window (keep last 24 hours, or last 5 GB)

**Files to touch:**
- `crates/rtspcam-core/src/config/mod.rs` — add `recording` settings
- `crates/rtspcam-pipeline/src/` — add muxer (could use `mp4-rs` or `moofly`)
- `crates/rtspcam-app/ui/` — record button in preview

#### 2.12 PTZ (Pan-Tilt-Zoom) Support
**Effort:** ~1 week
**Impact:** Medium — for cameras that support it (Hikvision, Dahua, Axis, etc.)

**What to do:**
- Detect PTZ capability during ONVIF discovery
- Add PTZ controls to the Edit dialog: directional pad + zoom slider
- Send PTZ commands via ONVIF `PtzContinuousMove` / `PtzStop`
- Store PTZ presets and recall them

**Files to touch:**
- `crates/rtspcam-onvif/src/client.rs` — add PTZ methods
- `crates/rtspcam-core/src/config/mod.rs` — add `ptz: Option<PtzConfig>`
- `crates/rtspcam-app/ui/dialogs.slint` — add PTZ controls
- `crates/rtspcam-engine/src/` — PTZ command dispatch

#### 2.13 Notification System
**Effort:** ~3 days
**Impact:** Low-medium — users want to know when cameras go offline

**What to do:**
- OS-native toast notifications when a camera disconnects / reconnects / errors
- Configurable notification settings (per-camera, global)
- Desktop notification via `notify-rust` (cross-platform)

**Files to touch:**
- `crates/rtspcam-core/src/config/mod.rs` — add notification settings
- `crates/rtspcam-app/src/ui/` — subscribe to status changes, show notifications

---

## 3. Audio Support Investigation

### 3.1 Current State

Audio is **explicitly out of scope** per the implementation plan (01-plan.md §1.1 and §5):
> *"Not in scope for now: audio (a virtual microphone is a separate problem), recording, and acting as an RTSP server."*

The `StreamConfig` model has no audio fields. The `retina` RTSP client depacketizes audio (AAC, G.711, G.723, L8/L16) but rtspcam never uses those audio frames. The `FrameSource` trait only deals with video. The IPC protocol has no audio wire format. The `FrameBus` is video-only.

### 3.2 What Cameras Typically Provide

Most IP cameras that output video also output audio. The common audio codecs in RTSP streams are:

| Codec | Prevalence | Notes |
|---|---|---|
| **AAC** | Very common | Modern cameras. retina already depacketizes AAC. |
| **G.711 (PCMU/PCMA)** | Very common | Legacy / VoIP cameras. retina already depacketizes. |
| **G.726** | Common | Older cameras. Not in retina's scope yet. |
| **Opus** | Rare | Mostly WebRTC cameras. |
| **G.729** | Rare | Some Hikvision/Dahua cameras. |
| **L16 (linear PCM)** | Common | Uncompressed, high bandwidth. retina supports. |

### 3.3 Existing Rust Audio Dependencies Available

| Crate | What it does | License | Fit for us |
|---|---|---|---|
| **retina** (already in use) | Depacketizes AAC, G.711, G.723, L16 from RTP | MIT/Apache-2.0 | ✅ Already integrated, audio frames already available |
| **rusty_aac** | Pure-Rust AAC-LC decoder, no FFI | Apache-2.0 | ✅ Ideal for AAC (most common camera codec) |
| **audio-codec** | G.711, G.722, G.729, Opus (via opus-rs) | MIT | ✅ G.711 is trivial, Opus via opus-rs |
| **hound** | WAV file I/O | MIT | ✅ For output format conversion |
| **cpal** | Cross-platform audio playback / capture API | MIT/Apache-2.0 | ⚠️ For testing, not for virtual mic |

### 3.4 Virtual Microphone Platforms

| Platform | Mechanism | Effort | Complexity |
|---|---|---|---|
| **Windows 11** | Core Audio: `IAudioRenderClient` + WASAPI loopback or a kernel-mode audio filter | Medium | **High** — needs a COM DLL (like vcam) running in Frame Server, or a kernel-mode filter. WASAPI loopback is the easier path: register as a WASAPI loopback device. |
| **Windows (legacy)** | KSAudio filter (kernel mode) or a software enumerator that presents a "virtual audio endpoint" | High | Very hard |
| **Linux** | PulseAudio/PipeWire `module-null-sink` + `module-pipe-source`; or `v4l2loopback` with audio (not standard) | Low-Medium | Medium |
| **macOS** | `CoreAudio` + `AudioUnit` (`kAudioUnitSubType_VirtualInput`); or `IOKit` audio property | Medium | Medium |

### 3.5 Effort Estimate: Audio Support

#### Phase A: Audio Ingestion & Decoding (3–5 days)
**Scope:** Decode audio from RTSP streams. No output yet — just get PCM samples.

1. **Extend `RtspSource`** (`rtspcam-pipeline/src/source.rs`)
   - Currently `pick_video_stream` selects only video. Modify to also detect and select audio tracks.
   - Create an `AudioSource` struct parallel to `RtspSource`.
   - Expose depacketized audio frames (AAC, G.711, L16 raw).

2. **Add audio decoders** (`rtspcam-pipeline/src/decode/`)
   - **AAC:** Add `rusty_aac` as an optional dependency. Implement `AacDecoder` that takes AAC-ECC frames and outputs 16-bit PCM.
   - **G.711:** Already trivial — it's the `audio-codec` crate, or a hand-written 30-line function (G.711 is just A-law/u-law companding).
   - **L16:** Already PCM — no decode needed, just byte-swap if needed.
   - Add an `AudioDecoder` trait parallel to `Decoder`.

3. **Add audio frame type** (`rtspcam-pipeline/src/`)
   - New `AudioFrame` struct: `data: Vec<i16>`, `sample_rate: u32`, `channels: u16`, `pts: Duration`
   - An `AudioBus` parallel to `FrameBus`

**Files touched:** `rtspcam-pipeline/` only. No platform or IPC changes yet.

#### Phase B: Platform Audio Output (1–2 weeks per platform)
**Scope:** Route decoded PCM to a virtual microphone device.

**Windows (hardest):**
- Option 1: **WASAPI Loopback** — register a virtual WASAPI endpoint. This requires a kernel-mode filter driver (same challenge as DirectShow virtual cam). Not practical.
- Option 2: **Core Audio virtual device via a COM DLL** — similar architecture to the vcam DLL. Create a WASAPI audio endpoint wrapper DLL that presents itself as a microphone. This runs in the user session (not Frame Server), so IPC is simpler (same process or named pipes).
- Option 3: **NAudio-style approach** — use `com-rs` or `windows` crate to implement `IAudioRenderClient`. The DLL runs in a separate process, communicates with the app over a pipe (like vcam).

Estimated effort for Windows: **5–7 days** (COM audio endpoint DLL, IPC protocol extension, PCM delivery loop).

**Linux:**
- Use `libpulse` or PipeWire client library to create a null sink and pipe audio to it.
- Or: `alsa` with a virtual device via `module-virtual-sink` / `module-virtual-source`.
- PipeWire is the modern approach: use `libpipewire` to create a virtual source.

Estimated effort for Linux: **3–5 days**.

**macOS:**
- Use `AudioUnit` with `kAudioUnitSubType_VirtualInput` to create a virtual input device.
- Similar architecture to the CoreMediaIO camera extension approach.

Estimated effort for macOS: **5–7 days**.

#### Phase C: IPC Protocol Extension (2–3 days)
**Scope:** Add audio frames to the app ↔ vcam protocol.

Current protocol:
```
Header (16 bytes) + Body
  Kind: Hello / SetFormat / Frame / Status / Goodbye
  Frame body: FrameHeader(32) + pixel data
```

Extension:
```
  Kind: AudioFrame (new)
  AudioFrame body: AudioHeader(16) + PCM data
    - sample_rate (u32)
    - channels (u16)
    - format (u16: 1=16-bit PCM, 2=32-bit float)
    - frame_count (u32): number of samples per channel
```

This goes in `rtspcam-ipc/src/protocol.rs`. The vcam DLL (Windows) or pusher (Linux/macOS) delivers the audio to the platform audio API.

#### Phase D: Integration (3–5 days)
**Scope:** Wire it all together.

- `CameraShared` gets an `AudioBus` alongside `FrameBus`
- `CameraManager` starts an audio pipeline alongside the video pipeline
- UI: show an "Audio" indicator per stream; toggle audio on/off; volume control
- Config: add `audio_enabled: bool` to `StreamConfig`
- Graceful degradation: if audio decode fails, the camera still works with video only

### 3.6 Total Audio Effort Summary

| Phase | Windows | Linux | macOS |
|---|---|---|---|
| A: Ingestion & decoding | ~5 days (shared) | ~5 days (shared) | ~5 days (shared) |
| B: Platform audio output | ~7 days | ~5 days | ~7 days |
| C: IPC extension | ~3 days (shared) | ~3 days (shared) | ~3 days (shared) |
| D: Integration | ~5 days (shared) | ~5 days (shared) | ~5 days (shared) |
| **Total per platform** | **~20 days** | **~18 days** | **~20 days** |
| **Shared work** | **~13 days** | | |
| **Platform-specific add-on** | +7 days | +5 days | +7 days |

**Grand total: ~26 days for Windows, ~24 days for Linux, ~26 days for macOS.**

### 3.7 Recommended Implementation Order

1. **Phase A (shared ingestion/decoding)** — proves audio from RTSP works. Low risk, high reward. Can be tested with `rtspcam-cli`.
2. **Windows Phase B** — the hardest platform. Use the same DLL architecture as vcam.
3. **Linux Phase B** — simpler (PipeWire/PulseAudio libraries are straightforward).
4. **IPC extension + Integration** — shared, done after Windows platform output works.
5. **macOS Phase B** — deferred until Windows and Linux are stable.

### 3.8 Risks

| Risk | Likelihood | Impact | Mitigation |
|---|---|---|---|
| Windows audio endpoint DLL is harder than vcam DLL | Medium | High | Spike first: build a minimal WASAPI loopback dummy DLL that a test app can read |
| Camera audio codec is uncommon (e.g., G.729) | Low | Medium | retina already handles AAC/G.711/L16. G.729 needs an extra crate |
| Apps don't use microphone input from virtual mics | Medium | High | Test early with Discord, Zoom, OBS. Some apps enumerate mic types and skip virtual ones |
| Audio drift / sync issues | Low | Medium | Timestamp audio frames alongside video frames; let the consumer handle sync |
| Platform audio APIs change between OS versions | Low | Low | Abstract behind traits, test on latest OS versions |

### 3.9 Alternative: Use ffmpeg-next for Audio (Fallback Path)

If building platform-specific audio output proves too hard, the `ffmpeg-next` fallback (already listed in 01-plan.md §2.3) can handle **decoding** every audio codec including Opus, AMR, Speex, etc. This trades a larger binary and build complexity for codec coverage. For a first implementation, `retina + rusty_aac + audio-codec` covers the vast majority of camera audio codecs.

---

## 4. Prioritized Roadmap

Based on the analysis, here is a recommended priority order:

| Priority | Item | Effort | Impact | Notes |
|---|---|---|---|---|
| **P0** | Verify cameras in Discord, Chrome, OBS (person-only) | 1 day | Critical | Blocks confidence in the core product |
| **P0** | Phase 7 installer on clean machine | 1 day | Critical | Blocks distribution |
| **P1** | Stream health dashboard (metrics in UI) | 2 days | High | Existing data, just expose it |
| **P1** | Phase 8: GPU decoding (DXVA) | 1–2 weeks | High | Needed for multi-camera scalability |
| **P2** | Audio ingestion & decoding (Phase A) | 5 days | High | Prerequisite for all audio work |
| **P2** | Dark mode | 2 days | Medium | Quick quality win |
| **P2** | Keyboard shortcuts | 1 day | Medium | Quick quality win |
| **P3** | Audio: Windows output | 7 days | Medium | Hardest platform-specific work |
| **P3** | Audio: IPC extension + integration | 8 days | Medium | Shared across platforms |
| **P3** | Audio: Linux output | 5 days | Medium | Easier than Windows |
| **P3** | Audio: macOS output | 7 days | Medium | Deferred until Win/Linux work |
| **P4** | Batch operations | 3 days | Medium | Organizational |
| **P4** | Stream groups / tags | 3 days | Medium | Organizational |
| **P4** | Snapshot export | 3 days | Medium | Fun feature |
| **P4** | RTSPS support | 5 days | Medium | Growing need |
| **P4** | Notification system | 3 days | Low-medium | Convenience |
| **P5** | Recording / clips | 1 week | Medium | Separate feature area |
| **P5** | PTZ support | 1 week | Medium | For PTZ cameras |
| **P5** | Import from other configs | 3 days | Low-medium | Niche |

---

## 5. Architecture Notes for Future Work

### 5.1 The Pusher Abstraction Is Ready for Audio
The `rtspcam_platform::push::Pusher` (added in Phase G) is designed to be a general-purpose frame delivery loop. While it currently handles only video frames, the same pattern applies to audio: a pusher that ticks at the audio sample rate and writes PCM samples to a `FrameSink`-like `AudioSink`. The `Pusher` could be generalized to a generic `Pusher<T>` or a new `AudioPusher` can be added following the same pattern.

### 5.2 The IPC Protocol Is Extensible
The protocol version field is `1` and the `Kind` enum uses `u16`, so new message kinds can be added without breaking existing implementations. Unknown kinds are skipped (forward-compatible). Adding `AudioFrame` as `Kind = 6` is safe.

### 5.3 Platform Boundary Is Clean
The `VirtualCameraBackend` trait and `PlatformDecoders` trait provide clean extension points. An `AudioEndpointBackend` trait could mirror this pattern, and `PlatformAudioDecoders` could mirror `PlatformDecoders`.

### 5.4 The FrameBus Pattern Works
The `watch::Sender<Option<Arc<Frame>>>` pattern is lightweight and well-tested. An `AudioBus` would follow the same pattern with `Arc<AudioFrame>`.

### 5.5 Edition 2024 & Rust 1.92
The project is already on `edition = "2024"` with `rust-version = "1.92"`, which is modern. This means newer language features are available (e.g., `if let` chains, macro by-example). Good positioning for adding new features without legacy constraints.

---

## 6. Summary

The RTSP Cam project is in a strong position: the core video pipeline is mature, the architecture is clean and modular, and the Linux/macOS groundwork is complete. The most impactful next steps are:

1. **Verify the core product** in real consumer apps (Discord, Chrome, OBS) — this is the single highest-impact item.
2. **Add GPU decoding** for scalability.
3. **Add audio** — the ingestion/decoding layer is relatively straightforward and leverages already-depacketized data from retina. The platform-specific output (especially Windows) is the challenging part.
4. **Small QOL improvements** (health dashboard, dark mode, shortcuts) deliver quick wins with minimal risk.

The codebase is well-structured for these additions: the platform trait boundaries, the extensible IPC protocol, and the pusher abstraction all make new features relatively easy to slot in.
