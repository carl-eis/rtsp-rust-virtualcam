# 15 — Comparison: our virtual cameras vs OBS Studio's

Date: 2026-10-10. Branch `feat/obs-virtualcam-comparison`. Continues nothing; a review of the
current virtual camera code (Windows: `rtspcam-vcam`, `rtspcam-vcam-mgr`, `rtspcam-ipc`,
`platform/src/windows/camera.rs`; Linux: `platform/src/linux/`, `push.rs`) against OBS
Studio's `master` (shallow clone of github.com/obsproject/obs-studio, same date):

| OBS file | What it is |
|---|---|
| `plugins/win-dshow/virtualcam-module/virtualcam-filter.cpp`, `virtualcam-module.cpp`, `placeholder.cpp`, `sleepto.c` | The DirectShow source filter DLL that apps load in-process |
| `plugins/win-dshow/shared-memory-queue.c`, `virtualcam.c`, `tiny-nv12-scale.c` | OBS's writer side and the shared-memory ring between them |
| `plugins/linux-v4l2/v4l2-output.c` | v4l2loopback writer |
| `plugins/mac-virtualcam/src/camera-extension/*.swift`, `obs-plugin/plugin-main.mm` | CoreMediaIO Camera Extension and the app side feeding its sink stream |

No code was changed. Items marked *verify* are inferences from reading code, not measured.

## 1. Summary

Ours does more than OBS's in most areas: one camera per stream (OBS has one, global), a
status-aware placeholder ("Connecting…", the error text) where OBS shows a fixed PNG,
bilinear scaling and per-consumer format conversion in the app where OBS scales nearest
neighbour in the DLL, automatic camera lifetime (session cameras vanish with the process),
and on Linux per-stream labelled devices with reader counting for on-demand streams.

What OBS has that we don't:

1. **Windows 10 support** (DirectShow filter). Ours needs Windows 11's `MFCreateVirtualCamera`.
2. **macOS** (Camera Extension, OBS 30+). Ours is planned as phase M ([11 §6](11-virtual-cameras-linux-macos-plan.md#6-phase-m--macos-coremediaio-camera-extension)).
3. **More consumer formats on Windows**: NV12, I420 and YUY2. We offer NV12 and RGB32.
4. **Precise frame pacing** inside the camera DLL (sleep then spin to the deadline) and
   **synthetic, evenly spaced sample timestamps**.
5. **Zero-copy-ish transport**: a shared-memory triple buffer instead of a pipe.
6. **60 fps** (macOS extension is fixed at 60; Windows follows OBS's output rate). We cap at 30.

Only 3 and 4 are cheap. 1 is a second implementation; 5 was already measured as unnecessary
([14 §3](14-progress-phase-8.md): the pipe write is about 1 ms per 720p RGB32 picture); 6 has
little value for IP cameras, which rarely exceed 30 fps.

## 2. How OBS does it, per OS

### Windows (DirectShow)

- `obs-virtualcam-module64.dll` is a DirectShow source filter registered under
  `CLSID_VideoInputDeviceCategory` with `MERIT_DO_NOT_USE` (plus a 32-bit build for 32-bit
  apps). Every app that opens the camera loads the DLL **into its own process** and gets its
  own `VCamFilter`. Registration happens once, as admin, by the installer (`regsvr32`).
- OBS writes NV12 into a named file mapping `OBSVirtualCamVideo`: a header (write/read index,
  state `STARTING/READY/STOPPING`, size, frame interval) and **three frame slots**, each with
  a 32-byte header holding the timestamp. The writer bumps `write_idx`, copies into slot
  `idx % 3`, then publishes `read_idx = idx`. Readers take whatever slot `read_idx` points
  at. No locks, no events; a torn read is possible in principle but needs the writer to lap
  the reader by two whole frames.
- The filter runs its own thread: `sleepto_100ns(cur_time += interval)` per frame, i.e.
  `Sleep(ms - 1)` and then a `Sleep(0)` spin on `QueryPerformanceCounter` until the deadline.
  Sample times are `filter_time .. filter_time + interval`, where `filter_time` starts at the
  graph clock and advances by exactly one interval per frame, so timestamps are perfectly even
  regardless of wake-up jitter.
- If the same `read_idx` is seen 10 times in a row, the source is considered gone and the
  placeholder shows (our equivalent: `STALE_AFTER`).
- Before OBS's output starts, the filter still has to advertise a format, so OBS writes
  `%APPDATA%\obs-virtualcam.txt` (`WxHxinterval`) each time it starts; the filter reads it
  when created. (We solve the same problem with the preferred-format attribute on the camera.)
- If the consumer chose a different size than OBS outputs, the filter scales nearest
  neighbour (`tiny-nv12-scale.c`, self-described as "kind of garbage") and converts to
  I420/YUY2 on the fly.
- If the filter is loaded inside `obs64.exe` itself, it switches its advertised format to
  OBS's output instead of scaling.

### Linux (v4l2loopback)

- On **Start Virtual Camera** OBS runs `pkexec modprobe v4l2loopback exclusive_caps=1
  card_label='OBS Virtual Camera'` if the module isn't loaded (through `flatpak-spawn --host`
  inside Flatpak), then scans `/dev/video*` in version order and takes the first device whose
  `device_caps` include `V4L2_CAP_VIDEO_OUTPUT` and that accepts `G_FMT/S_PARM/S_FMT` (YUYV at
  OBS's output size).
- **STREAMON workaround:** after `S_FMT` it tries `VIDIOC_STREAMON` on the output queue. The
  comment says it fails on everything except v4l2loopback 0.12.x–0.13.x; where it succeeds it
  keeps a static flag and does STREAMON at start and STREAMOFF at stop around its `write()`s.
- Frames go out with plain `write()`, retried for partial writes. No reader detection.

### macOS (Camera Extension)

- A system extension in `OBS.app` with one device, a **source stream** (what apps read) and a
  **sink stream** (what OBS writes), both fixed at 1920×1080 BGRA, 60 fps.
- The app finds the device by a UUID from its plist (`kCMIOHardwarePropertyDevices`, then
  `kCMIODevicePropertyDeviceUID`), takes the sink stream, `CMIODeviceStartStream`s it and
  `CMSimpleQueueEnqueue`s `CMSampleBuffer`s wrapping `CVPixelBuffer`s from an
  **IOSurface-backed `CVPixelBufferPool`** (so frames cross processes without copying).
- In the extension, a `DispatchSourceTimer` (`.strict`, zero leeway, user-interactive QoS)
  draws the placeholder at 60 fps while the sink isn't running; once the sink starts, a
  consume timer pulls from the sink and forwards to the source. Start/stop are reference
  counted per client.
- Activation goes through `OSSystemExtensionRequest`, handling `actionForReplacingExtension`
  (upgrades), "user approval required" and "will activate after reboot" results. It also
  removes the old DAL plugin if present.

## 3. Feature comparison

| | OBS | RTSP Cam |
|---|---|---|
| Cameras | One, global | One per stream |
| Windows mechanism | DirectShow filter in each app's process | MF custom media source in Frame Server (`MFCreateVirtualCamera`) |
| Windows versions | 10 and 11 | 11 only |
| 32-bit apps | Separate 32-bit DLL | Work for free (Frame Server is out of process) |
| MF-only apps (Windows Camera app, UWP) | Not listed *verify* | Listed |
| Shared between apps at once | Yes (each app reads the ring) | Yes (`MF_DEVICESTREAM_FRAMESERVER_SHARED`) |
| Windows formats | NV12, I420, YUY2 at OBS's size (any size via scaling) | NV12, RGB32 × 1080p/720p/480p × 30/15 fps |
| Scaling | Nearest neighbour in the DLL | Bilinear in the app (or on the GPU), once per format |
| Transport | Shared-memory triple buffer | Named pipe, one connection per source instance |
| Frame pacing in the camera | QPC sleep-then-spin to each deadline | `Condvar::wait_timeout` (see §4.1) |
| Sample timestamps | Synthetic, exactly one interval apart | `MFGetSystemTime()` at delivery |
| Placeholder | Fixed PNG (Windows: next to the DLL; macOS: in the bundle) | Rendered text matching the stream state |
| Camera lifetime | Registered permanently; placeholder when OBS is off | Exists only while the app runs (session lifetime) |
| Linux module loading | `pkexec modprobe` on demand | systemd unit at boot, picks options per module version |
| Linux device choice | First free output device | Labelled per stream; control device on 0.13+; reuse by label |
| Linux reader detection | None | Client-usage events (0.13+) drive on-demand streams |
| Linux STREAMON workaround | Yes | No (see §4.4) |
| Flatpak | `flatpak-spawn --host` | Not packaged as Flatpak |
| macOS | Camera Extension, IOSurface pool | Planned (phase M) |

## 4. Low-hanging fruit

Ordered by value for effort.

### 4.1 High-resolution waits in the delivery thread *(small; verify first)*

`stream.rs::deliver` paces with `Condvar::wait_timeout`, which on Windows becomes
`SleepConditionVariableSRW` with a millisecond timeout and is rounded to the process's timer
resolution. Since Windows 10 2004 the resolution is per process, and nothing in Frame Server's
svchost raises it, so the wait is quantised to about 15.6 ms ticks. `next_due` keeps the
*average* at 30 fps (which is all the Phase 8 bench measured), but individual frame gaps
likely alternate between about 31 and 47 ms — visible judder on motion. `server.rs` already
documents the same quantisation on the app side.

OBS avoids it with sleep-then-spin. The better Windows 10 1803+ answer, without spinning or a
global `timeBeginPeriod`: a waitable timer made with
`CreateWaitableTimerExW(.., CREATE_WAITABLE_TIMER_HIGH_RESOLUTION, TIMER_ALL_ACCESS)`, set to
the deadline, waited together with a stop event (`WaitForMultipleObjects`). Requests arriving
can keep using the condvar; only the "wait until `next_due`" step changes.

First measure: extend `tests/source_reader.rs` (it already collects sample times) or the
bench to record the arrival time of each sample at the consumer and report the spread of
intervals, not just the mean.

### 4.2 Evenly spaced sample timestamps *(trivial)*

`send_sample` stamps `MFGetSystemTime()` at the moment of delivery, so any wake-up jitter goes
straight into the timestamps. OBS stamps from a counter that advances by exactly one interval.
Do the same: take `MFGetSystemTime()` once when delivery starts, keep `start + n * interval`
alongside `next_due`, and re-anchor only when the real clock has drifted by more than an
interval or two (after a long gap between requests). Consumers that use timestamps for
playout (WebRTC in browsers, recorders) get smooth timing even before 4.1.

### 4.3 YUY2 (and I420) for Windows consumers *(small)*

OBS offers NV12, I420 and YUY2. YUY2 is what most USB webcams offer and what some older
DirectShow apps and video-conferencing stacks choose first; today such an app either gets
NV12/RGB32 through a Frame Server conversion or doesn't list a format it likes. Everything
needed exists already: `PixelFormat::Yuyv`/`I420` in the protocol, `nv12_to_yuyv`/I420 in
`rtspcam-pipeline::scale`, and placeholder rendering for both. The work is in
`formats.rs`: map `Yuyv` → `MFVideoFormat_YUY2` (stride `width * 2`) and `I420` →
`MFVideoFormat_I420`, read them back in `format_of`, and add them to `advertised` after NV12
(keep NV12 first: it is the native format and cheapest). Update the `twelve_formats…` test.
Mind the count: 4 pixel formats × 3 sizes × 2 rates = 24 types; that's fine for MF.

### 4.4 Check the v4l2loopback STREAMON workaround *(test only, maybe a few lines)* — issue #17

OBS calls `VIDIOC_STREAMON` on the output queue (and STREAMOFF on stop) where the module
accepts it, which its comment says is 0.12.x–0.13.x. We never do. [13 §1](13-progress-phase-l.md)
confirms that with exclusive caps, writing a frame is enough for the device to report *Video
Capture*, and we write a placeholder frame at claim time, so this is probably why OBS needed
it with readers that check streaming state (Chrome was the usual culprit). Add it to the
open browser checks in 13 §5: if Chrome on 0.12 doesn't list or open the camera, try
STREAMON after `S_FMT` (ignore `EINVAL`/`ENOTTY`, like OBS) and STREAMOFF before closing.

### 4.5 Reuse frame buffers in the DLL *(small, minor gain)*

Per delivered frame, the DLL allocates twice: `feed.rs` copies the pipe buffer into a new
`Arc<Vec<u8>>` (`data.to_vec()`), and `send_sample` allocates a new `MFCreateMemoryBuffer`.
At 1080p RGB32 that is 2 × 8 MB per frame in Frame Server. Keep a spare `Vec` in the feed
(swap with the shared slot, reuse when `Arc::get_mut` succeeds), and recycle samples with
`MFCreateVideoSampleAllocatorEx` (system memory) or a small pool keyed on
`IMFTrackedSample`. Not urgent: the bench shows the CPU cost is elsewhere.

## 5. Bigger items (not low-hanging)

- **Windows 10.** Would need a second DLL: a DirectShow source filter like OBS's, registered
  per camera (each with its own CLSID and friendly name, since DirectShow lists filters, not
  instances) and reading the same pipe. DirectShow cameras are invisible to MF-only apps
  *verify*, and Windows 10 is out of support since October 2025, so this is only worth it on
  user demand.
- **Shared memory instead of the pipe.** Already decided against in [14 §3](14-progress-phase-8.md).
  If it is ever revisited, OBS's design is the template: header + three slots, index published
  after the copy, plus a sequence number in each slot header so a reader can detect a torn
  read (OBS doesn't). Frame Server runs as LOCAL SERVICE, so the mapping needs an explicit
  DACL, like the log folder in `registry.rs`.
- **One clock instead of two.** The app's server ticks at the consumer's fps, and so does the
  DLL; two unsynchronised clocks at the same nominal rate occasionally produce a repeated
  frame followed by a skipped one. OBS has one clock (the filter's) and the writer just
  publishes. We could have the server send each new picture as soon as it exists (it already
  has `picture_signal`) and let only the DLL pace. That also cuts up to one frame of latency.
  Needs care with the "late picture" logic in `server.rs` and with the push backends, which
  share the rules (`push.rs`).
- **60 fps.** Requires lifting the 30 fps cap in config validation and the advertised list;
  only useful for sources that deliver it. *Built, with a "Source" size option: [18](18-progress-issue-13.md).*

## 6. Lessons for phase M (macOS), from OBS's extension

These fit the plan in [11 §6](11-virtual-cameras-linux-macos-plan.md#6-phase-m--macos-coremediaio-camera-extension) and are worth adding to M2/M3:

- Use an **IOSurface-backed `CVPixelBufferPool`** on the app side
  (`kCVPixelBufferIOSurfacePropertiesKey: {}`), so sink buffers cross into the extension
  without copies, and set `kCVPixelBufferPoolAllocationThresholdKey` (OBS: 5) so a stalled
  consumer can't grow the pool without bound.
- OBS hard-codes one format (1080p BGRA, 60 fps) and lets apps scale. We'd rather offer
  NV12 (`kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange`) at the stream's configured size,
  which avoids the BGRA conversion that costs 2.5 ms per 720p frame ([14 §3](14-progress-phase-8.md)).
- In the extension, draw the placeholder on a `.strict` zero-leeway dispatch timer while the
  sink isn't running; switch to forwarding when it is. Reference-count `startStream` /
  `stopStream` per client (OBS keeps a counter; several apps can read at once).
- Find our devices by **device UID** (`kCMIODevicePropertyDeviceUID`), not by name. With one
  device per stream we'll need the extension to create devices dynamically and the UID to
  carry the stream id — OBS's single fixed UUID from the plist won't do.
- Handle all `OSSystemExtensionRequest` outcomes in the UI: activated, needs user approval
  (point to System Settings), will complete after reboot, and replacement on upgrade
  (`actionForReplacingExtension` → `.replace`).
- Report the device's transport type as virtual (`kIOAudioDeviceTransportTypeVirtual`) and
  set a model name, as OBS does.

## 7. Things we already do better (keep them)

- Status-aware placeholder text instead of a fixed image; OBS can't say *why* there is no
  picture.
- Cameras that exist only while the app runs: nothing left in the camera list after
  uninstalling or when the app is off. OBS's camera stays listed and shows its placeholder.
- One camera per stream, each named after its stream, on Windows and Linux.
- Format negotiation done right: each consumer gets its own size and pixel format, converted
  once per format with a proper scaler, instead of nearest-neighbour scaling inside the
  camera.
- On Linux: no `pkexec` prompt at start (a boot unit loads the module with options chosen per
  version), devices labelled per stream and reused by label, and on-demand streams driven by
  reader events.
- Robustness inside Frame Server: every COM entry point is panic-guarded, and every message
  from the pipe is size-checked before allocation. OBS's filter trusts the shared-memory
  header (sizes and offsets) it reads from another process.
