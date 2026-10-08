# 01 — RTSP → Virtual Webcam for Windows (Rust): Research & Phased Implementation Plan

## 1. Goal

A Windows desktop app, written in Rust, that:

1. Lets the user set up **multiple RTSP streams** (IP cameras, NVRs, `mediamtx`, etc.) on the local network.
2. Shows **each stream as its own webcam** (for example "Front Door – Windows Virtual Camera") that normal apps can pick: **Discord**, Teams, Zoom, OBS, Chrome/Edge, and the Windows Camera app.
3. Works without the user having to install drivers by hand. One installer sets everything up.

Not in scope for now: audio (a virtual microphone is a separate problem), recording, and acting as an RTSP server.

### 1.1 Confirmed decisions

| # | Decision |
|---|---|
| D1 | **Windows 11 only** (build 22000+) for now. Windows 10 / DirectShow support is deferred and not scheduled. |
| D2 | **GUI toolkit: `winsafe`** (native Win32 controls). |
| D3 | **RTSP only at first.** The config keeps a `protocol` field (default and only value `rtsp`) so more protocols can be added later without changing the schema. |
| D4 | Streams are added with **IP/host, port, path, username and password**. **Config is stored as one JSON object.** |
| D5 | **Quitting the app (for example "Quit" from the tray menu) terminates the process completely.** No background service, helper process or leftover virtual cameras stay behind. |
| D6 | The app has a **"Minimize to tray" setting**. When it's on, minimizing hides the window and leaves only the tray icon. |

---

## 2. Research summary

### 2.1 How to make a virtual webcam on Windows

| Option | How it works | Pros | Cons |
|---|---|---|---|
| **A. Media Foundation virtual camera** (`MFCreateVirtualCamera`, Windows 11 build 22000+) | We write a COM **custom media source** DLL. It is registered in HKLM, and the app tells Windows to create a camera from it. Windows' **Frame Server** service loads the DLL and shares the camera with every app that asks for it. | Official, modern API, no kernel driver. Shows up for **Media Foundation and DirectShow** apps (VCamSample shows up in OBS's DirectShow "Video Capture Device", and GraphStudioNext lists it). Works with any app bitness (32 or 64 bit) because the DLL runs inside the service. Windows adds the "Windows Virtual Camera" suffix to the name and lists the camera in Settings → Cameras. Several cameras are supported. Exposed in `windows-rs` (`Win32::Media::MediaFoundation::MFCreateVirtualCamera`). | **Windows 11 only.** The DLL runs inside `svchost` (Frame Server, as *Local Service*) and Frame Server Monitor (as *Local System*), so it is a separate process from our app and we need IPC. The DLL must be in a folder those services can read (Program Files, not `C:\Users\...`). Registering it needs admin rights once. Hard to debug (you attach to the services; logging goes through ETW). |
| **B. DirectShow source filter** (OBS-virtualcam / `tshino/softcam` style) | A COM DLL registered as a "Video Input Device" filter. It is loaded **into each client app**, and frames come in through shared memory. | Works on Windows 10. Well-known approach. Discord supports it (OBS Virtual Camera). | **Invisible to Media Foundation-only apps** (Windows Camera app, UWP, newer apps). Bitness must match, so we would ship both x86 and x64 DLLs. Older technology. Several cameras means registering one CLSID per camera. |
| **C. Kernel / AVStream driver** | A real driver (like the old vcam samples). | Visible to everything. | Needs driver signing (EV cert plus Microsoft attestation), kernel risk, and a lot of work. **Rejected.** |

**Decision:** Use **Option A (MF virtual camera)** only, since Windows 11 is the target (D1). Option B is **deferred**. The frame-transport layer stays simple enough that a DirectShow filter could be added later if Windows 10 support is ever needed.

Key facts about the MF virtual camera API (from Microsoft docs and samples):

- `MFCreateVirtualCamera(type, lifetime, access, friendlyName, sourceId /* "{CLSID}" */, categories, count, &IMFVirtualCamera)`, then `Start()`, `Stop()`, `Remove()`.
- `MFVirtualCameraLifetime_Session`: the camera goes away when our `IMFVirtualCamera` object is released (that is, when the app exits). `MFVirtualCameraLifetime_System`: the camera stays until it is removed, but it does **not** survive a reboot by itself (Microsoft's sample re-creates cameras on startup).
- `MFVirtualCameraAccess_CurrentUser` does **not** need admin. `AllUsers` does.
- Calling it again with the same parameters reopens the same camera.
- The media source CLSID must be registered under `HKLM\Software\Classes\CLSID\{...}\InProcServer32` with `ThreadingModel=Both`. Per-user (HKCU) registration does not work, because the DLL is loaded by several processes running as different accounts.
- The media source must implement `IMFMediaSourceEx`, `IMFMediaEventGenerator`, `IMFGetService`, `IKsControl` (it can return "not supported" for everything), and have streams implementing `IMFMediaStream2`. The CLSID usually hands out an `IMFActivate` that creates the source. Most clients want **NV12**, and RGB32 is a common second option.
- Microsoft's sample has the media source talk back to a user-session tray app over a **named pipe**. That confirms named pipes work for IPC between the Frame Server and the app.
- Known problems: "Access denied" on `Start()` if the DLL sits in a folder the services can't read. "Mark of the Web" on downloaded binaries. Apps such as Discord often only list cameras at startup, so they may need a restart after a camera is added.

References:
- https://learn.microsoft.com/windows/win32/api/mfvirtualcamera/nf-mfvirtualcamera-mfcreatevirtualcamera
- https://github.com/microsoft/Windows-Camera/tree/master/Samples/VirtualCamera (official C++ sample: media source, MSI, tests)
- https://github.com/smourier/VCamSample (smallest C++ sample, NV12/RGB32, with D3D and CPU paths) and https://github.com/smourier/VCamNetSample (C# port, which shows the approach works outside C++)
- https://github.com/tshino/softcam (DirectShow fallback reference, MIT)

### 2.2 RTSP client (Rust)

| Option | Notes |
|---|---|
| **`retina`** (scottlamb) | Pure Rust, tokio async, MIT/Apache-2.0. Used in production by Moonfire NVR. Supports Basic and Digest auth, RTP over TCP (interleaved) and UDP (experimental). Depacketizes **H.264, H.265, MJPEG**, AAC and G.711. Includes workarounds for cheap cameras that break the spec. **Chosen.** |
| `ffmpeg-next` | Does RTSP, demuxing and decoding in one crate. But it needs FFmpeg dev libraries at build time, has LGPL/GPL licensing to manage, and makes the install bigger. **Fallback** if `retina` can't handle some camera. |
| `gstreamer-rs` | Very capable, but a large runtime dependency to ship on Windows. Rejected. |

### 2.3 Video decoding

| Option | Notes |
|---|---|
| **Media Foundation decoder MFTs** (via `windows-rs`) | Built into Windows. H.264 and MJPEG decoders are always there. **HEVC needs the "HEVC Video Extensions"** from the Store (or hardware support). Can use the GPU through DXVA/D3D11. Outputs **NV12**, which is the format the virtual camera wants. **Chosen as the main decoder.** |
| `openh264` crate | Cisco's software H.264 decoder. Simple API, BSD-licensed when built from source. **Software fallback** for H.264. |
| `ffmpeg-next` | Covers every codec. Same downsides as above. Optional fallback. |

Scaling and color conversion to the advertised output size (for example 1280×720 or 1920×1080): the MF **Video Processor MFT** (GPU) or a CPU path (`yuvutils-rs` / `dcv-color-primitives`, or a hand-written NV12 scaler).

### 2.4 GUI and app framework (native Windows toolkit)

The GUI uses a **native Windows toolkit**: real Win32 controls with Common Controls v6 visual styles, so the app looks and behaves like a standard Windows app (themed buttons, combo boxes, list views, tab order, high-DPI, accessibility, small exe).

| Option | Notes |
|---|---|
| **`winsafe`** | Safe, idiomatic Rust wrappers over Win32 and Common Controls (windows, dialogs, `ListView`, `ComboBox`, `Edit` with password style, `UpDown`, status bar, tray via `Shell_NotifyIcon`). Actively maintained. Supports both code-built windows and dialog **resources (`.rc`)**, so forms can be laid out in a resource editor. **Chosen (D2).** |
| `native-windows-gui` (NWG) | Popular, higher-level Win32 wrapper with derive macros and a layout system. Easy to use, but updated rarely. Not chosen. |
| Raw `windows` crate (Win32) | The most control, the most boilerplate. Used directly only for the custom preview control and anything `winsafe` doesn't cover. |
| WinUI 3 / WinAppSDK from Rust | Modern Fluent look, but Rust support is immature (the official `windows-app` crate was archived). Not chosen. |

**Live preview rendering:** a custom child window. It paints the latest decoded frame (converted to BGRA) with **Direct2D** (`ID2D1HwndRenderTarget` / `CreateBitmap`) through the `windows` crate, with `StretchDIBits` (GDI) as a simple fallback. Frames are pushed from pipeline threads by posting a custom window message (`WM_APP + n`), so the UI thread never blocks.

**Threading model:** the Win32 message loop owns the UI thread. Tokio (RTSP) and the decoder threads run separately and talk to the UI through channels plus `PostMessageW` notifications.

Supporting crates: `winsafe`, `windows` (Win32/COM/MF/Direct2D/DPAPI), `tokio`, `serde` + **`serde_json`** (config), `tracing` + `tracing-appender` (logs), `directories` (`%APPDATA%` paths), `uuid`, `embed-resource` (manifest, icons, dialog resources).

Network discovery (optional): ONVIF **WS-Discovery** over UDP multicast `239.255.255.250:3702`, then ONVIF `GetProfiles` / `GetStreamUri` (SOAP) to find RTSP URLs automatically. We can write a small client ourselves with `quick-xml` + `reqwest`. The available Rust ONVIF crates are immature.

### 2.5 Writing COM in Rust

- `windows` crate with `#[implement(...)]` and `#[interface]` macros to implement `IMFActivate`, `IMFMediaSourceEx`, `IMFMediaStream2`, `IKsControl`, `IMFGetService`, `IClassFactory`.
- The DLL is a `cdylib` that exports `DllGetClassObject`, `DllCanUnloadNow`, `DllRegisterServer`, `DllUnregisterServer`.
- MF helpers we will call instead of reimplementing: `MFCreateEventQueue` (event generator), `MFCreateAttributes`, `MFCreateMediaType`, `MFCreateStreamDescriptor`, `MFCreatePresentationDescriptor`, `MFCreateSample`, `MFCreateMemoryBuffer`, `MFAllocateSerialWorkQueue` / `MFPutWorkItem`.
- **Critical:** the DLL runs inside a Windows service. Every exported function and COM method must catch panics (`std::panic::catch_unwind`) and return an `HRESULT`. Never let a panic cross the FFI boundary. Keep the DLL small: **no tokio, no GUI crates**. Link the CRT statically (`+crt-static`) so there is no VC++ runtime to ship.

---

## 3. Architecture

```
┌──────────────────────────── User session ────────────────────────────┐
│  rtspcam.exe  (native Win32 GUI via winsafe + tray, runs in bg)       │
│                                                                       │
│   Config/UI ──► CameraManager ──► MFCreateVirtualCamera per camera    │
│                       │                                               │
│                       ▼   per camera (tokio task + decode thread)     │
│   retina RTSP ─► depacketize ─► MF decoder MFT ─► scale → NV12        │
│                                                     │                 │
│                                         FrameBus (latest frame)       │
│                                                     │                 │
│                          Named-pipe server  \\.\pipe\rtspcam\<id>     │
│                          (or shared memory, see Phase 0 spike)        │
└──────────────────────────────────────┬────────────────────────────────┘
                                       │ IPC (frames + control)
┌──────────────── Session 0 / Frame Server (svchost) ──────────────────┐
│  rtspcam_vcam.dll  (COM custom media source, one CLSID)               │
│   IMFActivate → MediaSource → MediaStream (NV12 / RGB32)              │
│   - on Start: connect to the pipe for its camera id                   │
│   - on RequestSample: hand back the newest frame; if none,            │
│     show a "No signal / app not running" image                        │
└──────────────────────────────────────┬────────────────────────────────┘
                                       ▼
                 Discord / Teams / OBS / Chrome / Camera app
```

### 3.1 Main design decisions

1. **Who decodes?** The **app** does. The DLL stays a thin frame forwarder. That keeps the risky code (networking and codecs) out of a system service, gives the GUI a free live preview, and lets one decoded stream feed both the preview and the camera.
2. **IPC channel.** Default plan: **named pipes**. The app is the server and sets a DACL that allows `LOCAL SERVICE` / `SYSTEM`. Named pipes work across sessions without `SeCreateGlobalPrivilege`, and Microsoft's sample uses them. 720p NV12 at 30 fps is about 41 MB/s, which a local pipe handles easily. **Alternative / later optimization:** a `Global\` shared-memory ring buffer created *by the DLL* (services have the privilege), with a DACL for the interactive user. Phase 0 tests both approaches.
3. **Telling cameras apart.** One DLL serves N cameras. Two options:
   - (a) Attach the camera id as a custom property or attribute on the `IMFVirtualCamera` and read it back in the source's activation attributes or device properties. **Must be verified in Phase 0.**
   - (b) **Fallback:** register a fixed pool of CLSIDs (for example 16 "slots") pointing to the same DLL, and map CLSID to slot to camera id.
4. **Streaming only when watched.** The DLL only connects when a client starts streaming. The app can then start RTSP for a camera only while someone is watching it (or while the preview is open). That saves bandwidth and CPU. This is a user setting: "Always connected" or "On demand".
5. **Output formats.** Advertise a small, predictable list: NV12 and RGB32 at **1920×1080, 1280×720, 640×480**, at 30 fps (plus 15 fps). Letterbox or scale the source to fit. Discord works best with 720p/30. Frame timing is driven by the source clock, and the last frame is repeated if the stream stalls, so clients never time out.
6. **Camera lifetime.** Always use `MFVirtualCameraLifetime_Session` with `MFVirtualCameraAccess_CurrentUser`. No admin is needed at runtime, and the cameras exist **only while `rtspcam.exe` is running** (D5). We don't use `System` lifetime, so quitting never leaves orphaned cameras. Optional **Start with Windows** (HKCU `Run` key) makes the cameras exist before Discord starts.
7. **App lifecycle (window, tray, quit).**
   - A single process, `rtspcam.exe`, contains the UI, tray icon, RTSP pipelines and IPC servers. There is **no Windows service and no helper process**.
   - The tray icon is shown whenever the app is running. Its menu has **Open**, **Pause all / Resume all** and **Quit**.
   - **Minimize** (title-bar button or Win+Down): if `minimize_to_tray` is **on**, the window is hidden (`SW_HIDE`), its taskbar button disappears, and a one-time balloon says "RTSP Cam is still running in the tray". If it's **off**, the window minimizes to the taskbar as normal. Double-clicking the tray icon, or choosing **Open**, restores the window and brings it to the front.
   - **Close (X)** and **tray → Quit** both **quit the app**. Shutdown order: stop accepting IPC, call `IMFVirtualCamera::Shutdown()` for each camera (the cameras disappear from Discord and other apps), stop the RTSP pipelines, flush logs, remove the tray icon (`NIM_DELETE`), then leave the message loop and **exit the process**.
   - **Shutdown watchdog:** if the clean shutdown hasn't finished within **3 seconds** (for example, a stuck network read), the app logs a warning and calls `std::process::exit` / `ExitProcess`, so the process always ends.
   - **Hard kill** (Task Manager, crash): Session-lifetime cameras are tied to the process, so Windows should remove them. This is checked in Phase 0 (S0.6). As a safety net, the app removes stale cameras with its CLSID at the next start.
   - The tray icon is re-added after Explorer restarts (handle the `TaskbarCreated` registered message).

### 3.2 Stream definition and JSON configuration

**Adding a stream.** The user enters the parts of the address and the app builds the URL. Nobody has to type a full RTSP URL (though advanced users can paste one):

| Field | Control | Default / rules |
|---|---|---|
| Name | Edit | Required. Used as the webcam name ("<Name> – Windows Virtual Camera"). Must be unique. |
| Protocol | ComboBox (drop-down list) | **`RTSP` (default and only entry for now, D3).** The control is there so the form layout won't change when more protocols are added (see §5 Future work). |
| IP address / host | Edit | Required. IPv4, IPv6 or hostname. Validated before saving. |
| Port | Edit + UpDown | Default **554** (taken from the protocol). The user can change it. |
| Path | Edit | Optional, for example `/Streaming/Channels/101`. Brand presets fill this in (Phase 6). |
| Username | Edit | Optional. |
| Password | Edit (`ES_PASSWORD`) with a "show" toggle | Optional. Stored encrypted (see below). |
| Transport | ComboBox | `TCP` (default) / `UDP`. |
| Output | ComboBox ×2 | Resolution `1280×720` (default) / `1920×1080` / `640×480`. FPS `30` (default) / `15`. |
| Fit mode | ComboBox | `Letterbox` (default) / `Crop` / `Stretch`. |
| On demand | CheckBox | Checked: only connect while an app is using the webcam or the preview is open. |
| Enabled | CheckBox | Checked by default. |

The final URL is `rtsp://{host}:{port}{path}`. Credentials are **not** put in the URL. They are passed to `retina` (Basic/Digest auth) separately, so they never show up in logs. A **Test connection** button runs a probe with the values currently in the form.

**Config file.** One JSON object at `%APPDATA%\RtspCam\config.json`, read and written with `serde` + `serde_json`:

```json
{
  "version": 1,
  "settings": {
    "start_with_windows": false,
    "minimize_to_tray": true,
    "log_level": "info"
  },
  "streams": [
    {
      "id": "6f1c2d3e-8a4b-4c5d-9e0f-112233445566",
      "name": "Front Door",
      "enabled": true,
      "protocol": "rtsp",
      "host": "192.168.1.50",
      "port": 554,
      "path": "/Streaming/Channels/101",
      "username": "admin",
      "password": "dpapi:AQAAANCMnd8BFdERjHoAwE/Cl+sBAAAA...",
      "transport": "tcp",
      "output": { "width": 1280, "height": 720, "fps": 30 },
      "fit_mode": "letterbox",
      "on_demand": true
    }
  ]
}
```

Rules:
- `protocol` is a serde enum. Its only variant for now is `rtsp`, which is also the `#[serde(default)]`. An unsupported value (for example `"http"`) makes that stream show as an error in the UI ("Unsupported protocol"). The rest of the config still loads.
- Missing fields use their defaults (`minimize_to_tray: false`, `start_with_windows: false`, `port: 554`, `transport: tcp`, etc.), so older or hand-written files still load.
- `version` allows schema migrations later. Unknown fields are kept or ignored, never treated as errors.
- **Passwords:** encrypted with Windows **DPAPI** (`CryptProtectData`, current-user scope) and stored as base64 with a `dpapi:` prefix. The JSON stays a single self-contained object, but only the same Windows user on the same machine can decrypt it. Export (Phase 6) leaves passwords out.
- Settings changed in the UI (for example toggling **Minimize to tray**) take effect immediately and are saved straight away.
- Writes are atomic (write `config.json.tmp`, then `ReplaceFileW` / rename) and the previous file is kept as `config.json.bak`.
- The app reloads the config when it changes (`notify` crate), so hand edits take effect. Invalid JSON is reported in the UI and the last good config stays in use.

### 3.3 Cargo workspace layout

```
rtsp-rust/
├─ Cargo.toml                 (workspace)
├─ crates/
│  ├─ rtspcam-core/           JSON config model (serde_json), Protocol enum + URL builder, DPAPI secrets, shared constants (CLSIDs, pipe names), errors
│  ├─ rtspcam-ipc/            frame protocol: header + NV12 payload, pipe/shm client & server, no tokio in the client side
│  ├─ rtspcam-pipeline/       retina ingest, decoder trait (MF / openh264), scaler, FrameBus, reconnect logic
│  ├─ rtspcam-vcam/           cdylib: COM media source (Rust, windows-rs), depends on core + ipc only
│  ├─ rtspcam-vcam-mgr/       safe wrapper over MFCreateVirtualCamera / Start / Stop / Remove
│  ├─ rtspcam-app/            native Win32 GUI (winsafe) + Direct2D preview + tray + CameraManager (bin: rtspcam.exe)
│  │   └─ res/                app.rc (dialogs, icons, version info), app.manifest (Common Controls v6, PerMonitorV2 DPI)
│  └─ rtspcam-cli/            dev tool: probe RTSP, register/unregister DLL, add/remove cameras, push test pattern
├─ installer/                 WiX (cargo-wix) or Inno Setup scripts
├─ tools/                     ETW trace config, debug scripts for FrameServer, test RTSP server setup
└─ documentation/
```

---

## 4. Phased implementation plan

Each phase ends with something we can demo and clear exit criteria.

### Phase 0: Feasibility spikes (de-risking)
**Goal:** Prove the risky parts work in Rust before building on them.

- [ ] **S0.1 Rust MF media source, hello world.** A `cdylib` with `windows-rs` that implements `IClassFactory` → `IMFActivate` → `IMFMediaSourceEx` + `IMFMediaStream2` + `IKsControl` + `IMFGetService`, producing an animated NV12 test pattern. Register it in HKLM from `C:\Program Files\RtspCam\`, and have a small exe call `MFCreateVirtualCamera` + `Start`.
  - Verify in: **Windows Camera app, Chrome/Edge (webcam test page), OBS (DirectShow), Discord (preview *and* the remote side of a call)**, Teams/Zoom if available. Check the issue VCamSample reported: the remote side in Teams sees nothing.
- [ ] **S0.2 Multiple cameras.** Create 2–3 cameras from one CLSID and confirm the source can tell which camera it is (custom attribute or property). If not, switch to the CLSID-slot pool.
- [ ] **S0.3 IPC across sessions.** From the Frame Server process, connect to a named pipe created by a normal (non-admin) user process, then push 1080p30 NV12 and measure latency and CPU. Also try the `Global\` shared memory created on the DLL side. Pick one.
- [ ] **S0.4 RTSP + decode.** Use `retina` with a real camera and with `mediamtx` + `ffmpeg` test streams (H.264 and H.265). Decode with the MF H.264 MFT to NV12. Measure end-to-end latency.
- [ ] **S0.5 Debugging setup.** ETW `tracing` layer (or a file logger under `C:\ProgramData\RtspCam\logs` with the right ACLs) for the DLL. Script to attach to the FrameServer / FrameServerMonitor services.
- [ ] **S0.6 Process exit = cameras gone.** With Session lifetime, check that cameras disappear from Discord and the Camera app (a) after a clean exit and (b) after killing the process in Task Manager. Also check what a consumer that is streaming at that moment sees (it should stop cleanly, not hang). Write down any cleanup the app has to do at the next start.
- [ ] **S0.7 winsafe check.** Build a minimal `winsafe` window with a `ListView`, a modal dialog, a tray icon with a context menu, hide-on-minimize, and a Direct2D child window painting frames posted from another thread.

**Exit:** A written decision record (`documentation/02-spike-results.md`) covering IPC choice, multi-camera mechanism, decoder choice, process-exit behavior, and a confirmed Discord compatibility result. **Go/no-go for Option A.**

### Phase 1: Workspace, foundations and CI
- [ ] Cargo workspace per §3.3, `rust-toolchain.toml` (stable, `x86_64-pc-windows-msvc`), `clippy`/`rustfmt` config.
- [ ] `rtspcam-core`: JSON config model per §3.2 (`%APPDATA%\RtspCam\config.json`, `serde_json`). Includes:
  - `Protocol` enum (only variant `Rtsp`, which is also the default) with `default_port()` (554) and a `StreamConfig::url()` builder (`rtsp://host:port/path`, IPv6 in brackets, no credentials in the URL).
  - `AppSettings { minimize_to_tray, start_with_windows, log_level }`.
  - Validation (unique names, valid host/IP, port range 1–65535).
  - DPAPI `Secret` type (`dpapi:` base64), serialized encrypted and redacted in `Debug`/logs.
  - Atomic save + `.bak`, schema `version` + migrations, file watching.
  - Unit tests: round-trip, defaults when fields are missing (protocol → rtsp), URL building, DPAPI round-trip.
- [ ] Logging with `tracing` (rolling files) and a panic hook.
- [ ] GitHub Actions on `windows-latest`: build, clippy, unit tests, artifact upload.
- [ ] Local test environment: `tools/test-rtsp/` with **mediamtx** + ffmpeg scripts that publish RTSP test-pattern streams (H.264 720p/1080p, H.265, MJPEG over RTSP, and one with username/password).

**Exit:** `cargo build --workspace` passes in CI. Config round-trips to and from disk. Test RTSP streams are reachable.

### Phase 2: RTSP ingest and decode pipeline (headless)
- [ ] `rtspcam-pipeline::source`: `retina` session per camera (DESCRIBE/SETUP/PLAY). Pick the video stream. TCP by default with UDP as an option. Basic/Digest auth.
- [ ] Reconnect with exponential backoff and jitter. Detect stalls (no frames for N seconds). Status events: `Connecting`, `Streaming{fps, bitrate, res, codec}`, `Error(msg)`, `Backoff(t)`.
- [ ] `Decoder` trait with implementations:
  - `MfDecoder`: H.264 / HEVC / MJPEG MFT, software mode first (synchronous MFT), NV12 output. Handle `MF_E_TRANSFORM_STREAM_CHANGE` (resolution changes).
  - `OpenH264Decoder`: fallback.
- [ ] `Scaler`: NV12 → NV12 at the target size (letterbox/crop/stretch), plus NV12 → RGBA for the preview. CPU first.
- [ ] `FrameBus`: per camera, latest frame only (`Arc<Frame>` behind `arc-swap` or `watch`), so slow readers never hold up the decoder.
- [ ] `rtspcam-cli probe <url>` (codec/resolution info) and `rtspcam-cli view <url>` (dump frames or show FPS).
- [ ] Unit tests (decoder on recorded Annex-B samples, scaler golden images) and integration tests against mediamtx.

**Exit:** The CLI runs 4 streams at once for 1 hour with stable memory, recovers after the RTSP server restarts, and adds less than 150 ms of pipeline latency.

### Phase 3: Virtual camera media source (production quality)
Building on S0.1:
- [ ] Full state machine for the source and stream: `Start`/`Stop`/`Pause`/`Shutdown`, `SetStreamState`, stream selection and deselection. Follow the fixes listed in Microsoft's sample (return a *reference* to source attributes, use a dedicated work queue per stream, treat reselection with the same media type as a no-op).
- [ ] Media types: NV12 + RGB32 × {1080p, 720p, 480p} × {30, 15} fps, with correct `MF_MT_FRAME_SIZE`, `MF_MT_FRAME_RATE`, `MF_MT_DEFAULT_STRIDE`, `MF_MT_INTERLACE_MODE`, and so on.
- [ ] Sample timing: monotonic timestamps from `MFGetSystemTime`, fixed frame duration, repeat the last frame if no new one has arrived.
- [ ] **IPC client** (from `rtspcam-ipc`): connect when streaming starts, reconnect quietly, and request the media type the client chose so the app scales to exactly that size. When there is no connection, show a built-in "No signal" or "RTSP Cam not running" image (pre-rendered NV12 compiled into the DLL).
- [ ] Optional D3D11 path: if `MF_SOURCE_READER_D3D_MANAGER` / `IMFDXGIDeviceManager` is provided, use DXGI surface samples (`IMFSampleAllocatorControl`). Fall back to memory buffers.
- [ ] Panic safety around every COM entry point. Logs through ETW or the ProgramData file.
- [ ] Self-registration (`DllRegisterServer` writes HKLM CLSID keys with `ThreadingModel=Both`). `DllUnregisterServer` also removes the cameras it created.
- [ ] `rtspcam-vcam-mgr`: safe Rust API, `VirtualCamera::create(name, id, lifetime) -> Handle` (Drop = Shutdown), plus `list()` and `remove()`. Runs on a background thread, never the UI thread.
- [ ] Test harness: load the source in-process (`CoCreateInstance`), read through `IMFSourceReader`, check media types and frame cadence. Similar to Microsoft's `VirtualCameraTest`.

**Exit:** `rtspcam-cli vcam add --name Test --pattern` creates a camera that streams smoothly in Camera app, Chrome, OBS and **Discord** for 1 hour. Start/stop/reselect cycles leak nothing (check the Frame Server process handle count and memory).

### Phase 4: Host integration (CameraManager)
- [ ] `CameraManager` in the app owns, per enabled camera: the pipeline, the IPC server, and the `IMFVirtualCamera` handle. It reacts to config changes (add/edit/remove/enable/disable) without restarting the app.
- [ ] On-demand mode: the IPC "client connected / format requested" message starts the RTSP pipeline, and "client gone" stops it after a grace period (say 10 s).
- [ ] Format negotiation: the DLL tells the app which media type the consumer picked, and the pipeline scales to it.
- [ ] Health and status model shared with the UI (state, fps in/out, consumer connected, last error).
- [ ] Single-instance guard (named mutex). A second launch just brings the existing window forward.
- [ ] **Quit = process exit (D5)**: `CameraManager::shutdown()` implements the order in §3.1 #7 (stop IPC, `IMFVirtualCamera::Shutdown` for every camera, cancel the tokio runtime with `shutdown_timeout`, join the decode threads). A 3-second watchdog thread force-exits if shutdown hangs.
- [ ] Startup cleanup: remove any stale virtual cameras registered with our CLSID (for example after a crash) before creating the configured ones.

**Exit:** With the app running headless (config file only), 3 configured RTSP cameras show up as 3 webcams. Discord can switch between them. Unplugging a camera shows "No signal", and the picture recovers on its own when it comes back. Quitting removes all 3 cameras and **no `rtspcam.exe` process remains** (checked in Task Manager), including while Discord is actively using a camera.

### Phase 5: Desktop UI
Native Win32 UI built with `winsafe` (Common Controls v6 manifest, PerMonitorV2 DPI awareness, keyboard navigation).

- [ ] **Main window**:
  - Toolbar/buttons: **Add stream**, Edit, Remove, Start/Stop, Settings.
  - Menu: File → Settings…, Quit. Help → Open logs folder, About.
  - `ListView` (report view) of streams: Name, Address (`rtsp://host:port/path`), Status (Connecting / Streaming 30 fps / Error / Idle / In use by app), Enabled checkbox.
  - **Preview pane**: custom child window with Direct2D rendering of the selected stream (GDI fallback), plus info text (codec, source resolution, fps, bitrate).
  - Status bar: number of active cameras, any error count.
- [ ] **Add/Edit Stream dialog** (modal, laid out in `app.rc` or in code), with the fields from §3.2:
  - Name, **Protocol combo (RTSP, preselected, the only option for now)**, **IP address/host**, Port (default 554), Path, **Username**, **Password** (masked, with show toggle), Transport, Output resolution/FPS, Fit mode, On demand, Enabled.
  - "Advanced: paste full URL" field that accepts an `rtsp://user:pass@host:port/path` URL and splits it into host/port/path/username/password. Any other scheme is rejected with a clear message.
  - Inline validation (OK stays disabled until the form is valid, with error text next to the bad field).
  - **Test connection** runs a background probe and shows a result: success (codec, resolution, snapshot thumbnail) or a friendly error (wrong credentials, unreachable host, timeout, unsupported codec).
  - OK saves to `config.json` (atomic) and `CameraManager` picks up the change right away.
- [ ] Remove with a confirmation dialog (also removes the virtual camera).
- [ ] **Tray icon** (`Shell_NotifyIconW` through `winsafe`/`windows`), shown while the app runs:
  - Tooltip: "RTSP Cam – N cameras active".
  - Left double-click restores the window. Right-click opens the context menu: **Open**, **Pause all / Resume all**, separator, **Quit**.
  - **Quit** runs the full shutdown in §3.1 #7 and **ends the process**.
  - Re-registered on `TaskbarCreated` (Explorer restart).
- [ ] **Minimize to tray** (D6): handle `WM_SIZE` with `SIZE_MINIMIZED` (or `WM_SYSCOMMAND`/`SC_MINIMIZE`). When `settings.minimize_to_tray` is true, hide the window (`ShowWindow(SW_HIDE)`) so it leaves the taskbar and Alt-Tab. Restoring uses `ShowWindow(SW_RESTORE)` + `SetForegroundWindow`. When false, minimize normally.
- [ ] **Close (X)** quits the app the same way tray → Quit does.
- [ ] **Settings dialog**: ☐ **Minimize to tray** (default off), ☐ Start with Windows (HKCU `Run` key, launched with `--minimized`, which starts hidden in the tray if minimize-to-tray is on, otherwise minimized to the taskbar), log level. Changes are saved to `config.json` straight away.
- [ ] Friendly errors: wrong credentials (401), unreachable host, unsupported codec (with an HEVC Store extension hint), Frame Server access denied (with a hint about the install path).
- [ ] Discord tip shown after adding a camera: "Restart Discord if the camera doesn't appear in Settings → Voice & Video."

**Exit:** A non-technical user can add a stream by entering just an IP address, username and password (protocol stays on the RTSP default), test it, rename it, remove it, and use it in Discord. Everything is saved to `config.json` and survives a restart. With **Minimize to tray** on, minimizing hides the window to the tray, and it can be restored from there. **Tray → Quit** and **Close (X)** both end the process, and the cameras disappear from Discord.

### Phase 6: Discovery and convenience
- [ ] ONVIF WS-Discovery scan ("Find cameras on network"), then a credentials prompt, then `GetProfiles`/`GetStreamUri` to fill in the RTSP URL (main or sub stream).
- [ ] URL templates for common brands (Hikvision, Dahua, Reolink, Amcrest, Tapo, UniFi Protect) as a fallback when ONVIF isn't available.
- [ ] Import/export config JSON (passwords left out).
- [ ] Per-camera extras: rotate/flip, crop region, text overlay (name/time), "freeze last frame vs. show No signal" on disconnect.

**Exit:** On a LAN with ONVIF cameras, the user can add a camera without typing its URL.

### Phase 7: Packaging and installation
- [ ] Installer (WiX via `cargo-wix`, or Inno Setup) that requires admin **only at install time**:
  - Installs to `C:\Program Files\RtspCam\` (readable by the Frame Server services).
  - Registers the media source CLSID(s) in HKLM (registry table, not `regsvr32`, so repair and uninstall are clean).
  - Optional "Start with Windows".
  - Uninstall: close any running `rtspcam.exe` (Restart Manager / close request, which triggers the normal quit), run `rtspcam.exe --remove-all-cameras` as a safety net, then unregister the CLSID and delete files. Optionally keep the user config.
- [ ] Code-sign the exe, DLL and MSI (avoids SmartScreen and Mark-of-the-Web trouble). Embed version info resources (`winres`/`embed-resource`) and an app manifest (DPI awareness, `asInvoker`).
- [ ] Release build profile: LTO, `panic = "unwind"` in the DLL (needed for `catch_unwind`), `+crt-static`.
- [ ] Check OS build ≥ 22000 in the installer (launch condition) and in the app at startup. On older Windows, show "RTSP Cam requires Windows 11" and exit.
- [ ] Release pipeline in CI that produces a signed MSI on tag.

**Exit:** Clean install, then add a camera, use it in Discord, reboot (the camera comes back if autostart is on), uninstall. Tested on a fresh Windows 11 VM with no leftover registry entries, files or cameras.

### Phase 8: Performance, robustness and compatibility
- [ ] GPU path: D3D11 hardware decoding (DXVA) plus Video Processor MFT scaling, and DXGI samples into the virtual camera when the consumer provides a D3D manager. Target under 3% CPU per 1080p stream on a mid-range machine.
- [ ] Shared-memory IPC if pipe throughput or latency turns out to be a bottleneck (decided in Phase 0).
- [ ] Soak tests: 8 cameras for 24 h, network flapping, sleep/resume, user switching, Frame Server restarts.
- [ ] Compatibility matrix in `documentation/`: Discord (desktop and browser), Teams (new), Zoom, Google Meet in Chrome/Edge, OBS, Windows Camera, Slack. Cover preview **and** what the remote party sees.
- [ ] Fuzz the IPC frame parser and the config loader. Check sizes and bounds on every frame the DLL receives.
- [ ] Crash reporting (minidumps for the app; ETW for the DLL).

---

## 5. Future work (not scheduled)

- **More protocols** behind the existing `Protocol` selector and JSON field: RTSPS (RTSP over TLS: check `retina` support, otherwise use the `ffmpeg-next` fallback) and HTTP/HTTPS MJPEG (`reqwest` multipart stream into the MF MJPEG decoder). Possibly RTMP/SRT.
- **Windows 10 support** with a DirectShow source filter DLL (x64 + x86, as in `softcam` / OBS-virtualcam) reading frames through a shared-memory ring. Not visible to Media Foundation-only apps.
- Audio from the stream as a virtual microphone.

---

## 6. Risks and mitigations

| Risk | Impact | Mitigation |
|---|---|---|
| Implementing a full MF media source in Rust via `windows-rs` is new ground (few or no public Rust examples) | High | Phase 0 spike. Port closely from VCamSample / Microsoft sample / VCamNetSample (the C# port shows non-C++ implementations work). Keep a C++ fallback DLL in mind if Rust COM gets stuck. |
| Discord (or Teams) doesn't fully accept MF virtual cameras (for example, the remote side sees black, as VCamSample reports for Teams) | High | Check early in S0.1, including the remote view. Test all advertised media types. If it can't be fixed, the DirectShow approach (§5) becomes the fallback. |
| Telling cameras apart inside a single CLSID | Medium | CLSID slot pool fallback (S0.2). |
| Cross-session IPC permissions (Frame Server as LocalService ↔ user app) | Medium | Named pipe with an explicit DACL. DLL-created `Global\` shared memory as the alternative (S0.3). |
| A panic or crash in the DLL takes down Frame Server for every camera on the system | High | `catch_unwind` everywhere, no heavy dependencies in the DLL, fuzzing, fixed-size validated frames. |
| HEVC cameras on machines without the HEVC extension | Medium | Detect and explain in the UI. Recommend H.264 sub-streams. Optional openh264/ffmpeg fallback (H.264 only for openh264). |
| Camera quirks (bad SDP, missing SPS/PPS, odd timestamps) | Medium | `retina` already works around many. Fall back to `ffmpeg-next` for problem cameras if needed. |
| Native Win32 UI takes more code than immediate-mode toolkits (layout, DPI, custom preview control) | Medium | `winsafe` + dialog resources for layout. Keep the UI small (one main window plus one dialog). Isolate the Direct2D preview in its own module. |
| Passwords in the JSON config | Medium | DPAPI encryption per user, redacted in logs, never put into URLs, left out of exports. |
| Apps only list cameras at startup. Because cameras only exist while the app runs (D5), starting the app after Discord means Discord must be restarted | Low | "Start with Windows" setting (start hidden in the tray). UI hint after adding a camera. |
| The process doesn't exit on Quit (stuck network I/O, a thread that won't join, COM calls blocking) | Medium | Fixed shutdown order, tokio `shutdown_timeout`, 3-second watchdog that force-exits, checked in Phase 4/5 exit criteria. |
| Cameras left behind after a crash or hard kill | Low | Session lifetime (checked in S0.6) plus startup cleanup of stale cameras with our CLSID. |
| Windows 11 only | Low (accepted, D1) | OS check in the installer and the app. Windows 10 is listed in Future work. |

## 7. Open questions for the team

1. Expected number of cameras at once and typical resolutions/codecs (H.264 vs. H.265)? This affects the GPU-path priority.
2. Should the **Close (X)** button also go to the tray when "Minimize to tray" is on? The current plan is that X always quits (matching D5), and only minimize goes to the tray.
3. Are dialogs in `.rc` resources (visual editor) preferred over building the layout in code with `winsafe`?
4. Distribution: internal use only, or public release (code-signing certificate, auto-update)?
5. Is audio from the RTSP stream (virtual microphone) wanted in a later version?

## 8. Rough effort estimate (one experienced Rust/Windows developer)

| Phase | Estimate |
|---|---|
| 0 Spikes | 1.5–2 weeks |
| 1 Foundations | 3–4 days |
| 2 Pipeline | 1.5–2 weeks |
| 3 Media source | 2–3 weeks |
| 4 Host integration | 1 week |
| 5 UI | 1.5 weeks |
| 6 Discovery | 1 week |
| 7 Packaging | 1 week |
| 8 Hardening | 2 weeks |

**MVP (Phases 0–5 + a minimal Phase 7): about 9–11 weeks.**
