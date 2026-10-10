# 17 — Comparison: RTSP Cam vs RealTimeWebCam

Date: 2026-10-10. Continues [15](15-obs-virtualcam-comparison.md), the OBS comparison. A review
of our code against RealTimeWebCam (shallow clone of github.com/AndreaGreco/RealTimeWebCam,
`master` at `5b508c0`, 2026-10-06, release 1.4.0), mainly its `CLAUDE.md`,
`docs/PIANO_LATENZA.md` (its latency plan, in Italian), `CHANGELOG.md`,
`VirtualCamera/MediaStream.cpp`, `Shared/VCamFrameChannel.h` and
`RTCamNative/FfmpegRtspSource.cpp`.

No code was changed. Items marked *verify* are inferences from reading code, not measured.

## 1. Summary

RealTimeWebCam is built the same way as our Windows side: a Windows 11 `MFCreateVirtualCamera`
camera, RTSP decoded in the app (FFmpeg/libav, d3d11va when it can), and a Frame Server DLL that
only re-serves the app's frames and shows a placeholder when the app is gone. Its scope is
narrower: Windows only, one camera, one URL, NV12 only, C# WinForms + C++, no automated tests.

What it has that we don't, in order of value:

1. **A latency cap that catches up to live** (§3.1). We have no ceiling on how far behind live
   a stream can fall.
2. **An optional camera that stays registered** after the app closes (§3.2), so Zoom and Teams
   keep it selected.
3. **No lost request tokens** in the DLL (§3.3). We drop one when a sample can't be sent.
4. **Delivery when a frame arrives**, not on a fixed tick (§3.4): what 15 §5 calls "one clock
   instead of two", built and measured.

Plus a negative result worth keeping (§3.5) and a few smaller ideas (§3.6).

## 2. How RealTimeWebCam works

- **App** (`RTVirtualCamera.exe`, C#) calls into `RTCamNative.dll` (C++/CLI), which creates the
  camera, sets its configuration as MF attributes (`SetString`/`SetUINT32`; their notes say
  `SetBlob` isn't reliably forwarded to the source) and starts an FFmpeg producer thread.
- **Frame channel:** a `Global\` file mapping created by the Frame Server (it has
  `SeCreateGlobalPrivilege`, the user doesn't) with a DACL granting Interactive Users read and
  write. A one-page header plus three page-aligned NV12 slots, sized for 3840×2160 so a size
  change never needs a new mapping. The header is published under a seqlock; the reader checks
  after copying that the writer hasn't come back to the slot it copied (torn-read check, one
  retry).
- **Decoding straight into the slot:** a d3d11va frame that is already NV12 at the target size is
  downloaded with `av_hwframe_transfer_data` into the shared slot (with a non-owning `buf[0]`,
  or libav allocates its own); everything else goes through `sws_scale` into the slot.
- **Frame-ready event:** an auto-reset `Global\` event, also created by the Frame Server, that
  the app signals after each publish. The DLL delivers on it (§3.4).
- **Stats channel:** a second mapping going the other way (Frame Server → app) with rendered,
  repeated and received frame counts, copy time and drift, shown in the app twice a second.
- **Placeholder:** Direct2D text ("Camera IP non connessa") or a PNG the user picks, shown before
  the first frame and when the producer's heartbeat is more than 2 s old.

## 3. What we can learn

### 3.1 A latency cap that catches up to live *(medium; measure first)*

**Ours:** `rtspcam-pipeline/src/pipeline.rs` sends encoded frames to the decode thread over a
bounded `channel(8)`, so a slow decoder slows the network read down instead of dropping
compressed frames (which would corrupt the picture until the next key frame). That's right for
picture quality, but nothing limits how far behind live a stream falls. Over TCP the delay can
build up in socket buffers and on the camera without anyone noticing. Our `StreamStats::latency`
is the time from a frame's last packet to its decoded picture, so it can't see this either.

**Theirs** (`FfmpegRtspSource`, their plan step 9, changelog 1.3.0):

- They anchor wall-clock time to the stream's timestamps at the first frame and compute each
  frame's **lag** behind live. The same number is shown in the app as *Drift*: steady means a
  fixed latency, rising means latency is building up.
- **Level 1, catch-up:** when a frame is more than 350 ms behind (`LatencyCapMs`, a user
  setting), they keep decoding but stop presenting (no scale, no copy) and set
  `skip_frame = AVDISCARD_NONREF` (skip frames nothing else depends on) until the lag drops to
  half the cap. No visible freeze, just a short jump forward. Frames decoded but not shown still
  count as alive for the stall detector, so catching up never triggers a reconnect.
- **Level 2, resync:** only if catch-up lasts more than 1 s, or the lag grows by another cap's
  worth, they skip to the next key frame, flush the decoder and re-anchor. That freezes the
  picture for a whole key-frame interval (2–4 s on many cameras), so it's the last resort.

**For us:**

1. Add a drift figure to `StreamStats`: RTP timestamp (retina gives it per frame) against
   arrival time, anchored at the session's first frame and re-anchored on reconnect. Show it in
   the stream details. This alone tells us whether we have the problem.
2. Only if drift does build up: catch-up in the decode thread. Our `Decoder` trait has no
   "decode but don't output" or "skip non-reference frames" mode, so level 1 needs either that
   (OpenH264, Media Foundation and D3D11 decoders each differ) or the cheaper option of not
   publishing to the `FrameBus` while behind (saves scaling and conversion, not decoding).
   Level 2 already exists in pieces: `wait_for_key` in `DecodeLoop`.

With hardware decoding at about 5 % of a core per 1080p camera ([14 §3](14-progress-phase-8.md)),
a slow decoder is unlikely to be the cause on Windows. Cameras or networks that send in bursts,
and software decoding on Linux and macOS (OpenH264 only), are the more likely ones *verify*.

### 3.2 An optional camera that stays registered *(medium; product decision)*

**Theirs** (changelog 1.4.0, `CLAUDE.md` "Persistent camera"):

- A setting, off by default, registers the camera with `MFVirtualCameraLifetime_System` instead
  of `_Session`, still with `MFVirtualCameraAccess_CurrentUser`, so **no admin rights** are
  needed. The camera survives app restarts and reboots; Zoom and Teams keep it selected and show
  the placeholder until the app runs again.
- On stop or app close they only stop the producer and release the camera object without
  `Stop`/`Remove`. The next start re-creates it with the same source id (which re-attaches to the
  existing camera), sets the attributes and `Start`s it; it is restarted (`Stop` + `Start`,
  interrupting whoever has it open) only when size, frame rate or overlay changed since last
  time.
- Turning the setting off removes the camera when idle. **Uninstalling removes it** through a
  deferred, impersonated MSI custom action (`RTVirtualCamera.exe --remove-camera`), impersonated
  because the camera belongs to the user; upgrades keep it.

**Ours:** session lifetime only (`rtspcam-vcam-mgr/src/lib.rs`), which [15 §7](15-obs-virtualcam-comparison.md#7-things-we-already-do-better-keep-them)
lists as a strength: nothing left behind when the app is off or uninstalled. Their opt-in shows
we can offer both. The cost for us is higher because we have a camera per stream:

- Deleting a stream, or turning the option off for it, has to remove its camera, including when
  the app was closed at the time (remember registered cameras in the config and clean up at the
  next start).
- The Inno Setup uninstaller needs to remove them as the user (`runasoriginaluser`), not as admin.
- The DLL's placeholder already says "RTSP Cam is not running"; that is what a persistent
  camera would show.

### 3.3 Never lose a request token *(small; bug fix)*

`rtspcam-vcam/src/stream.rs`, `deliver`: when `send_sample` fails with anything other than
`MF_E_SHUTDOWN`, we log the error and the request token is gone. Frame Server keeps only a few
requests open at once, so each lost one permanently reduces how many remain; after enough of
them the stream stops *verify*.

Their fix (plan step 2): deliver something whenever possible (a failed copy still sends the
sample), and when a sample really can't be sent, queue `MEError` with the HRESULT so Frame Server
closes and reopens the stream cleanly instead of waiting forever. For us the realistic failure is
`one_off_sample` failing to allocate when the pool is empty. Fix: on a non-shutdown error, queue
`MEError` (as they do), or retry once with a one-off sample before giving up.

### 3.4 Deliver when a frame arrives *(medium; measured by them)*

[15 §5](15-obs-virtualcam-comparison.md#5-bigger-items-not-low-hanging) proposed letting only one
side keep time. They built it (plan step 6, changelog 1.3.0):

- The app signals the frame-ready event after each publish. The DLL waits on it with a thread
  pool wait, re-armed after each signal.
- A single "frame due" flag, not a counter: a burst of frames from the producer never becomes a
  burst of samples.
- A thread pool timer at the nominal rate is the fallback: it makes a frame due only when no
  event came for about 1.5 intervals (app closed, so the placeholder still flows at the nominal
  rate).

They report about 16 ms less latency on average (half a frame) and no more periodic repeated or
skipped frames from 25 and 29.97 fps cameras beating against a 30 fps timer. We have the same
beat: a 25 fps camera on our 30 fps delivery repeats a picture every sixth sample.

This pulls against [16 §1](16-progress-issue-12.md#1-what-was-done): our evenly spaced timestamps
assume samples go out on a steady clock. They stamp `MFGetSystemTime()` at delivery. A design for
us would deliver on arrival (the app already has `picture_signal` in `rtspcam-ipc/src/server.rs`),
keep the high-resolution timer as the fallback, and stamp samples from the camera's own
timestamps (or arrival times smoothed by `Timeline`) instead of a fixed interval. It changes the
rules the Linux push backend shares (`push.rs`), so it needs its own plan.

### 3.5 Negative result: don't decline repeated frames

They tried not re-sending a frame the consumer had already received by returning
`MF_E_SAMPLEALLOCATOR_EMPTY` from the request path. It upset Frame Server's retry timing and was
reverted (`Shared/VCamStats.h`, `declinedFrames`). Every request should get a sample, even a
repeated one. We already do this; this note is so nobody tries it.

### 3.6 Smaller ideas

- **Camera settings tip** (their README): low-delay encoding, baseline profile or no B-frames,
  a key frame about every second, and the camera's real frame rate rather than an upsampled one.
  A short paragraph in our README would help users more than most code changes. Shorter key-frame
  intervals also shorten our reconnect and decoder-swap times (both wait for a key frame).
- **Frame-counter overlay** burned into the video (a diagnostic setting): with a stopwatch in
  front of the camera, it makes end-to-end latency and repeated frames visible in the call app
  itself.
- **Automatic UDP → TCP fallback:** they start on UDP and switch to TCP when a connection gives
  no frames within the socket timeout, and show which transport actually carries the frames. We
  default to TCP and let the user pick, so this matters less.
- **User-picked offline image:** they let the user choose a picture for when there is no video.
  Our placeholder shows the stream status, or the last frame; a custom image could be a third
  choice in **Picture...**.

## 4. Checked and not needed

- **Installer stops Frame Server during install and upgrade:** ours already does
  (`installer/rtspcam.iss`, `StopFrameServer`).
- **Decoding straight into shared memory / shared memory instead of the pipe:** decided against
  in [14 §3](14-progress-phase-8.md).
- **Write-only `Lock2DSize` on the output sample:** matters when Frame Server hands the source
  D3D11 texture samples. Our pool is created without a device manager, so samples are in system
  memory.
- **libav's `stimeout` → `timeout` rename** and their double watchdog on blocking calls: FFmpeg
  specific. retina calls are already bounded by our connect timeout and stall detection.

## 5. Things we already do better (keep them)

- One camera per stream; Linux virtual cameras; ONVIF discovery and brand presets; picture
  options; on-demand streams.
- 24 advertised formats (NV12, YUY2, I420, RGB32 × 3 sizes × 2 rates) where they offer one NV12
  type at the stream's size.
- A placeholder that says why there is no picture (connecting, the error text, app not running).
- Panic guards on every COM entry point and size checks on every pipe message before allocating.
- Automated tests, including the frame-gap test through `IMFSourceReader`
  ([16 §2](16-progress-issue-12.md#2-measurements)). They have none.

## 6. Suggested next steps

1. §3.3, the lost token: a few lines in `stream.rs`. Do it now.
2. §3.1 step 1, a drift figure in `StreamStats`: tells us whether we need the rest of §3.1.
3. §3.6, the camera settings tip in the README.
4. §3.2 and §3.4: decide whether we want them, then write a plan for each.
