# 00 — Index: where to find what

The entry point for the documentation folder. Start here, read the documents marked
**current** for the state of the project, and use the jump tables to go straight to a topic.
Section numbers (§) refer to the headings inside each document.

## 1. Where the project stands (as of 2026-10-10, v1.2.1)

- **Windows:** plan phases 0–7 are built; phase 8 (GPU path, soak, fuzzing, crash reports) has
  not started. The GPU path was re-planned for the cross-platform code in
  [08.1](08.1-Plan-Phase-8.md). Several exit criteria still need a person (cameras in Discord/OBS/Camera app,
  installer on a clean VM).
- **Cross-platform:** the OS-specific code is in `rtspcam-platform`, and the UI is Slint instead
  of winsafe. The app runs on Windows and Linux, and it builds and passes its tests on macOS.
  This work was merged to `master` in PR #1.
- **Linux and macOS virtual cameras:** planned in [11](11-virtual-cameras-linux-macos-plan.md).
  Phase G (shared frame pusher, placeholder in `rtspcam-ipc`, YUYV/I420) is done
  ([12](12-progress-phase-g.md), 2026-10-10). Phase L is done too ([13](13-progress-phase-l.md),
  2026-10-10): Linux has v4l2loopback cameras and `.deb`/`.rpm` packages, checked against the
  real module with ffmpeg but not yet in browsers or call apps. Since then SIGTERM quits cleanly
  and an edited stream keeps its device ([13 §8](13-progress-phase-l.md#8-fixes-after-phase-l-2026-10-10-featlinux-sigterm-and-device-removal)). Phase M is not started, so macOS
  still uses `UnsupportedCameras`.

## 2. The documents

| # | Document | Kind | Covers | Read it for |
|---|---|---|---|---|
| 01 | [01-plan.md](01-plan.md) | Plan | Goal, research, architecture, phases 0–8 | Why things are built the way they are; the phase checklists and exit criteria |
| 02 | [02-progress.md](02-progress.md) | Progress | Phase 1 (workspace, config, CI, test server) | How `rtspcam-core` works (config, `Secret`, migrations, watcher, logging) |
| 03 | [03-progress-phases-2-3.md](03-progress-phases-2-3.md) | Progress | Phase 2 (RTSP + decode), Phase 3 (DLL media source) | Pipeline design, IPC protocol, how the DLL works, soak results |
| 04 | [04-session-2026-10-09.md](04-session-2026-10-09.md) | Session log | Commits of phases 2–3, lessons learned | Pitfalls with Media Foundation, retina, the Docker test server and the build |
| 05 | [05-progress-phases-4-5.md](05-progress-phases-4-5.md) | Progress | Phase 4 (CameraManager), Phase 5 (winsafe UI) | How the camera manager reconciles, demands and shuts down; how camera identity was solved (§6) |
| 06 | [06-checkpoint-2026-10-09.md](06-checkpoint-2026-10-09.md) | Checkpoint | Phase status table, known problems | The status of every phase on one page (Windows) |
| 07 | [07-progress-phase-6.md](07-progress-phase-6.md) | Progress | Phase 6 (ONVIF, brand templates, import/export, picture options) | ONVIF discovery, the `picture` config object, the `WM_CREATE` dialog bug |
| 08 | [08-progress-phase-7.md](08-progress-phase-7.md) | Progress | Phase 7 (Inno Setup installer, release workflow) | Installer decisions and what is still unverified |
| 08.1 | [08.1-Plan-Phase-8.md](08.1-Plan-Phase-8.md) | Plan | Phase 8 GPU path after the cross-platform refactor | **Current** GPU plan: measure first, D3D11 decode with copy-back, "Use hardware decoding" setting with device shown, early downscale; GPU frames into the camera deferred |
| 09 | [09-cross-platform-plan.md](09-cross-platform-plan.md) | Plan | `rtspcam-platform`, `rtspcam-engine`, Slint | The traits at the OS boundary, the crate layout, where `cfg` is allowed |
| 10 | [10-progress-cross-platform.md](10-progress-cross-platform.md) | Progress | Steps A–F of 09, CI on three OSes | **Current** crate layout and trait table; what still needs checking on each OS |
| 11 | [11-virtual-cameras-linux-macos-plan.md](11-virtual-cameras-linux-macos-plan.md) | Plan | v4l2loopback (Linux), CoreMediaIO extension (macOS) | **Next work:** phase M (G and L are done) |
| 12 | [12-progress-phase-g.md](12-progress-phase-g.md) | Progress | Phase G of 11 | **Current** `Pusher`/`FrameSink`, shared placeholder, new pixel formats, on-demand without reader info |
| 13 | [13-progress-phase-l.md](13-progress-phase-l.md) | Progress | Phase L of 11 | **Current** v4l2loopback facts per version, the Linux backend, `.deb`/`.rpm`, what still needs a person |

### Which documents are current

Later documents correct earlier ones. When they disagree, the later one wins:

| Older statement | Replaced by |
|---|---|
| "Not verified" lists and next steps in 02, 03, 04, 05 §4 | [06 §4](06-checkpoint-2026-10-09.md#4-verification-state), then [10 §4](10-progress-cross-platform.md#4-not-verified) |
| Camera identity through `AddProperty`, CLSID-pool fallback (03, 05) | String attributes on `IMFVirtualCamera`: [05 §6](05-progress-phases-4-5.md#6-update-first-run-through-frame-server-s02-and-s06-resolved) |
| Startup sweep of stale cameras (`remove_stale`, 05 §2) | Removed; session cameras vanish even on a hard kill ([06 §3](06-checkpoint-2026-10-09.md#3-decisions-made-since-0304)) |
| winsafe UI, GDI preview, GDI overlay text, tray balloon (05, 06, 07) | Slint UI, `ab_glyph` overlay, Slint tray: [10 §1–2](10-progress-cross-platform.md#1-what-was-done) |
| Code locations in `rtspcam-app/src/` (`manager.rs`, `source.rs`, `overlay.rs`, `backend.rs`, ...) | Moved to `rtspcam-engine` and `rtspcam-platform`: [09 §5](09-cross-platform-plan.md#5-where-each-file-goes) |
| Placeholder pictures in `rtspcam-vcam/src/placeholder.rs`; "nothing built yet" in 11 | `rtspcam_ipc::placeholder`, phase G done: [12](12-progress-phase-g.md) |
| `rust-version` 1.85 (02, 05, 06) | 1.92 ([10 §2](10-progress-cross-platform.md#2-decisions-beyond-the-plan)) |
| 01 Phase 8 GPU bullet (D3D11 decode, Video Processor, DXGI samples into the camera) | Staged plan in [08.1](08.1-Plan-Phase-8.md): CPU NV12 stays the shared frame type; DXGI samples deferred |
| CI "never run" / "unknown" (02–08) | Green on Windows, Linux and macOS ([10 §7](10-progress-cross-platform.md#7-step-e-ci)) |
| Linux packaging through `/etc/modules-load.d` and `/etc/modprobe.d` (11 §4 L3) | A boot service that picks options by module version: [13 §3](13-progress-phase-l.md#3-decisions-beyond-the-plan) |
| Next free document number (04, 05, 06, 07) | 14. A Phase 0 decision record was never written; [06 §1](06-checkpoint-2026-10-09.md#1-phase-status) explains where each spike was covered. |

## 3. Reading order by task

| If you want to... | Read |
|---|---|
| Get up to speed quickly | This file, then [10](10-progress-cross-platform.md), then [06](06-checkpoint-2026-10-09.md) |
| Resume work | [10 §6](10-progress-cross-platform.md#6-how-to-resume), then [13 §7](13-progress-phase-l.md#7-how-to-resume) and [11 §7](11-virtual-cameras-linux-macos-plan.md#7-order-size-and-what-each-phase-delivers) |
| Build and run on Windows | [06 §6](06-checkpoint-2026-10-09.md#6-how-to-resume), [02 §6](02-progress.md#6-developer-setup-notes) (Build Tools, build from PowerShell) |
| Build and test on Linux | [10 §7](10-progress-cross-platform.md#7-step-e-ci) (Docker command), [10 §5](10-progress-cross-platform.md#5-known-gaps) (system libraries) |
| Do the checks that need a person | [10 §4](10-progress-cross-platform.md#4-not-verified), [08](08-progress-phase-7.md#not-verified-needs-a-person-ideally-on-a-vm), [07 §5](07-progress-phase-6.md#5-not-verified-needs-a-person), [06 §4](06-checkpoint-2026-10-09.md#4-verification-state) |
| Add a virtual camera backend for macOS (Linux: [13](13-progress-phase-l.md)) | [11](11-virtual-cameras-linux-macos-plan.md), [09 §4.1](09-cross-platform-plan.md#4-traits), the `Pusher` in [12 §1](12-progress-phase-g.md#pusher-rtspcam_platformpush) |
| Start Phase 8 (Windows hardening) | [08.1](08.1-Plan-Phase-8.md), [01 Phase 8](01-plan.md#phase-8-performance-robustness-and-compatibility), [03 §6](03-progress-phases-2-3.md#6-known-issues), [06 §5](06-checkpoint-2026-10-09.md#5-known-problems-and-rough-edges) |
| Change the config format | [01 §3.2](01-plan.md#32-stream-definition-and-json-configuration), [02 §3](02-progress.md#3-what-exists-crate-by-crate), [07 picture options](07-progress-phase-6.md#per-camera-picture-options) |
| Touch Media Foundation or retina code | [04 §3](04-session-2026-10-09.md#3-lessons-learned-the-non-obvious-parts) first |

## 4. Jump table by topic

### Product and architecture

| Topic | Where |
|---|---|
| Goal, scope, decisions D1–D6 | [01 §1](01-plan.md#1-goal), [01 §1.1](01-plan.md#11-confirmed-decisions) |
| Why MF virtual cameras, retina, MF decoders | [01 §2](01-plan.md#2-research-summary) |
| Process architecture (app ↔ pipe ↔ DLL in Frame Server) | [01 §3](01-plan.md#3-architecture), [01 §3.1](01-plan.md#31-main-design-decisions) |
| App lifecycle: tray, minimize to tray, quit = process exit | [01 §3.1](01-plan.md#31-main-design-decisions) item 7 |
| Crate layout (current) | [10 §1](10-progress-cross-platform.md#crate-layout), dependency arrows in [09 §3](09-cross-platform-plan.md#3-crate-layout) |
| Crate layout (original, Windows only) | [01 §3.3](01-plan.md#33-cargo-workspace-layout) |
| Where OS `cfg` is allowed, and the CI check | [09 §3](09-cross-platform-plan.md#where-cfg-may-appear), [10 §7](10-progress-cross-platform.md#7-step-e-ci) |
| Future work (Windows 10, RTSPS, HTTP MJPEG, audio) | [01 §5](01-plan.md#5-future-work-not-scheduled) |
| Risks | [01 §6](01-plan.md#6-risks-and-mitigations), [09 §7](09-cross-platform-plan.md#7-risks), [11 §8](11-virtual-cameras-linux-macos-plan.md#8-risks) |

### Platform boundary (`rtspcam-platform`)

| Topic | Where |
|---|---|
| Trait per OS at a glance | [10 §1 traits table](10-progress-cross-platform.md#traits-all-re-exported-from-rtspcam_platform) |
| `VirtualCameraBackend` | [09 §4.1](09-cross-platform-plan.md#4-traits) |
| `SecretStore` (DPAPI, keyring, key file) | [09 §4.2](09-cross-platform-plan.md#4-traits) |
| `FileReplace`, `Autostart`, `SingleInstance`, `FrameTransport`, `PlatformDecoders` | [09 §4.3–4.7](09-cross-platform-plan.md#4-traits) |
| Startup order in `main()` | [09 §4.9](09-cross-platform-plan.md#4-traits) |
| Old file → new file mapping | [09 §5](09-cross-platform-plan.md#5-where-each-file-goes) |
| Where the cross-platform brief was wrong | [09 §2](09-cross-platform-plan.md#2-where-the-code-disagrees-with-the-brief) |

### Configuration and secrets

| Topic | Where |
|---|---|
| Stream fields and JSON format | [01 §3.2](01-plan.md#32-stream-definition-and-json-configuration) |
| Defaults, unknown fields, `Protocol::Unsupported`, validation | [02 §3](02-progress.md#3-what-exists-crate-by-crate) |
| Atomic save, `.bak`, migrations, file watcher | [02 §3](02-progress.md#3-what-exists-crate-by-crate) |
| Locked passwords (another user, PC or OS) | [02 §3](02-progress.md#3-what-exists-crate-by-crate), [09 §4.2](09-cross-platform-plan.md#4-traits) |
| `picture` object (rotate, flip, crop, overlay, freeze) | [07 §2](07-progress-phase-6.md#per-camera-picture-options) |
| Import and export | [07 §2](07-progress-phase-6.md#import-and-export) |
| Paths (config, logs) | [02 §3](02-progress.md#3-what-exists-crate-by-crate), [02 §5](02-progress.md#5-decisions-and-deviations) |

### RTSP pipeline and decoding

| Topic | Where |
|---|---|
| Ingest → decode → FrameBus, backoff, stall detection, `StreamState` | [03 §2](03-progress-phases-2-3.md#how-the-pipeline-works) |
| `ErrorKind` and user hints | [03 §2](03-progress-phases-2-3.md#how-the-pipeline-works) |
| Decoders per OS (MF on Windows, OpenH264 elsewhere) | [10 §1](10-progress-cross-platform.md#traits-all-re-exported-from-rtspcam_platform), [10 §5](10-progress-cross-platform.md#5-known-gaps) |
| H.265 CRA workaround, MF output-buffer bug | [03 §5](03-progress-phases-2-3.md#5-decisions-and-deviations), [04 §3](04-session-2026-10-09.md#3-lessons-learned-the-non-obvious-parts) |
| Latency, 4-stream run, 1-hour soak and memory drift | [03 §4.2](03-progress-phases-2-3.md#42-phase-2-exit-criteria) |
| GPU path plan (D3D11 decode, early downscale) | [08.1](08.1-Plan-Phase-8.md) |
| Colour range, CPU scaling, MJPEG gaps | [03 §6](03-progress-phases-2-3.md#6-known-issues) |

### Windows virtual camera (DLL and IPC)

| Topic | Where |
|---|---|
| MF virtual camera API facts | [01 §2.1](01-plan.md#21-how-to-make-a-virtual-webcam-on-windows) |
| Pipe protocol (header, messages, validation, DACL) | [03 §3](03-progress-phases-2-3.md#ipc-rtspcam-ipc) |
| DLL structure, pacing, placeholders, logs | [03 §3](03-progress-phases-2-3.md#the-dll-rtspcam_vcamdll); the placeholder now lives in `rtspcam-ipc` ([12 §1](12-progress-phase-g.md#placeholder-rtspcam_ipcplaceholder)) |
| Camera identity: attributes on `IMFVirtualCamera` | [05 §6](05-progress-phases-4-5.md#6-update-first-run-through-frame-server-s02-and-s06-resolved), [06 §3](06-checkpoint-2026-10-09.md#3-decisions-made-since-0304) |
| Developer install of the DLL, `vcam` CLI checks | [03 §4.3](03-progress-phases-2-3.md#43-phase-3), [04 §6](04-session-2026-10-09.md#6-how-to-resume) |
| Never `Path::exists` on a pipe | [05 §2](05-progress-phases-4-5.md#changes-outside-the-app-crate) |

### Camera manager and UI

| Topic | Where |
|---|---|
| Reconciler, on-demand grace, pause, shutdown order and watchdog | [05 §2](05-progress-phases-4-5.md#how-it-works) |
| Slint UI: sheets instead of dialogs, tray, preview worker, `rfd` | [10 §1](10-progress-cross-platform.md#ui-slint-118), [09 §2](09-cross-platform-plan.md#2-where-the-code-disagrees-with-the-brief) |
| Overlay font (Inter via `ab_glyph`) | [10 §2](10-progress-cross-platform.md#2-decisions-beyond-the-plan) |
| ONVIF discovery and the Find cameras flow | [07 §2](07-progress-phase-6.md#onvif-rtspcam-onvif-new-crate), [07 Find cameras](07-progress-phase-6.md#find-cameras-dialog) |
| Brand URL templates | [07 §2](07-progress-phase-6.md#brand-templates) |
| Historical winsafe notes (crash in `DrawTextW`, `WM_CREATE` vs `WM_INITDIALOG`) | [05 §3](05-progress-phases-4-5.md#things-worth-knowing), [07 §3](07-progress-phase-6.md#3-bug-found-on-the-way-dialogs-never-initialised) |

### Build, CI, packaging

| Topic | Where |
|---|---|
| Developer setup on Windows | [02 §6](02-progress.md#6-developer-setup-notes) |
| Test RTSP server (mediamtx + ffmpeg in Docker) | [02 §3](02-progress.md#tooling), [04 §3](04-session-2026-10-09.md#3-lessons-learned-the-non-obvious-parts) |
| CI matrix, toolchain and fontconfig fixes | [10 §7](10-progress-cross-platform.md#7-step-e-ci) |
| Windows installer (Inno Setup), signing, release workflow | [08](08-progress-phase-7.md#decisions) |
| Linux packages and v4l2loopback dependency | [13 §2](13-progress-phase-l.md#packaging-installerlinux-cratesrtspcam-appcargotoml-releaseyml) (built), [11 §5](11-virtual-cameras-linux-macos-plan.md#5-can-the-linux-release-install-v4l2loopback-automatically) (why Recommends) |
| Linux virtual cameras (v4l2loopback backend, versions, testing on WSL2's kernel) | [13](13-progress-phase-l.md) |
| macOS app bundle, DMG, notarization | [11 §6](11-virtual-cameras-linux-macos-plan.md#6-phase-m--macos-coremediaio-camera-extension) |

## 5. Adding a document

Give it the next free number (`13-...`), state at the top what it continues and its date, and
add a row to §2 here. If it makes something in an earlier document out of date, add a row to
"Which documents are current" as well.
