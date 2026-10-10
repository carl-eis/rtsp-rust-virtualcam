# 16 — Progress: virtual camera pacing, timestamps, YUY2/I420, buffer reuse (issue #12)

Date: 2026-10-10. Continues [15 §4](15-obs-virtualcam-comparison.md#4-low-hanging-fruit), the
low-hanging fixes from the OBS comparison, tracked in GitHub issue #12. Items 4.1, 4.2, 4.3 and
4.5 are built. Item 4.4 (the v4l2loopback STREAMON check) needs a person and moved to issue #17; see §3.

## 1. What was done

### Frame pacing on a high-resolution timer (15 §4.1), `crates/rtspcam-vcam/src/stream.rs`

- The delivery thread waits for each frame slot on a waitable timer made with
  `CreateWaitableTimerExW(.., CREATE_WAITABLE_TIMER_HIGH_RESOLUTION, TIMER_ALL_ACCESS)`, with a
  normal waitable timer as the fallback before Windows 10 1803. No spinning and no
  `timeBeginPeriod`.
- It waits with `WaitForMultipleObjects` on the timer and a manual-reset stop event. `Delivery`'s
  `Drop` sets the event, so stopping the stream still ends the wait at once. Requests still
  arrive through the existing queue and condition variable; only the "wait until `next_due`"
  step changed.
- If the event or the timer can't be created, the thread paces with the condition variable as
  before (coarser, but working). Every handle is closed by a small owning `Handle` type.
- App side (`rtspcam-ipc/src/server.rs`): **left as it is**. Its tokio interval has the same
  ~15.6 ms rounding, but it no longer sets the camera's cadence: the DLL keeps the newest
  frame and sends it on its own even slots. App-side jitter only decides which source picture
  is the newest at a slot, and the RTSP camera's own clock and network jitter already do that.
  A fix would need a timer thread bridged into tokio, behind a `cfg` in a cross-platform crate.
  That isn't contained, and the gain is small. If repeated or skipped pictures turn out to
  matter, the better change is to send each new picture when it is published (the
  `picture_signal` the server already uses for late pictures) instead of on a tick.

### Evenly spaced timestamps (15 §4.2), `stream.rs`

- Samples are stamped `start + n * 10_000_000 / fps` (100 ns units, multiplied before dividing so
  30 fps doesn't drift) by a small `Timeline`. It re-anchors to `MFGetSystemTime()` only when the
  clock is more than two intervals away from the next slot, for example after a long gap between
  requests. The sample duration is unchanged.

### YUY2 and I420 (15 §4.3), `crates/rtspcam-vcam/src/formats.rs`

- `Yuyv` maps to `MFVideoFormat_YUY2` (default stride `width * 2`) and `I420` to
  `MFVideoFormat_I420` (stride `width`). `format_of` reads both back. `advertised` lists NV12,
  YUY2, I420, then RGB32: 24 types, NV12 first, and the preferred size and rate still move the
  NV12 type to the front.
- The app side needed no change: `rtspcam-engine`'s `CameraSource::next_frame` and the CLI's
  test pattern camera already convert to both formats (`nv12_to_yuyv`, `nv12_to_i420`), and
  `rtspcam_ipc::placeholder` already renders both. Only comments that said "the Windows DLL never
  asks for it" were updated (`rtspcam-ipc/src/protocol.rs`, `rtspcam-pipeline/src/scale.rs`).

### Buffer reuse (15 §4.5), `crates/rtspcam-vcam/src/feed.rs`, `stream.rs`

- The feed keeps the frame that the newest one replaced and copies the next frame into it when
  `Arc::get_mut` shows that the delivery thread has let go of it. It holds a frame only while
  sending it, so this is the normal case. Otherwise a new buffer is allocated as before.
- Samples come from `MFCreateVideoSampleAllocatorEx` without a device manager, so in system
  memory, the way Microsoft's virtual camera sample does it. The pool starts with 2 samples and
  grows to at most 8. A sample goes back to the pool only when Frame Server releases it, so the
  pool can't hand out a sample that is still in use. If the pool is empty or can't be created,
  the sample is allocated one-off as before. A recycled sample's attributes are cleared before it
  is reused, so a stale request token can't leak into the next sample. Data goes in with
  `IMF2DBuffer::ContiguousCopyFrom`, which handles the buffer's row pitch.

## 2. Measurements

`crates/rtspcam-vcam/tests/source_reader.rs::frame_gaps_are_even` creates the media source in
the test process (no registration), reads 90 samples of the 720p30 placeholder through
`IMFSourceReader`, records when each one reaches the reader, and prints the gaps
(`cargo test -p rtspcam-vcam --test source_reader frame_gaps -- --nocapture`). The test process,
like Frame Server, doesn't raise the timer resolution. This machine runs Windows 11 Pro 26200.

| | Mean gap | Min | Max | Worst deviation from 33.33 ms | Gaps within 2 ms |
|---|---|---|---|---|---|
| Before (condvar timeout), 3 runs | 33.33–33.39 ms | 29.7–29.9 ms | 47.6–47.8 ms | 14.3–14.4 ms | 17–28 of 89 |
| After (high-resolution timer), 8 runs | 33.33–33.34 ms | 32.5–33.0 ms | 33.8–34.0 ms | 0.5–0.8 ms | 89 of 89 |

The test asserts that at least 80 % of the gaps are within 2 ms of the interval (loose, so a
busy machine doesn't fail it), and that the timestamps step by 333 333 or 333 334 ticks.

A temporary log line, removed afterwards, confirmed that every sample in the source-reader tests
came from the pool. That covers NV12, RGB32, YUY2 and I420.

## 3. Not verified (needs a person)

- **15 §4.4, v4l2loopback STREAMON (Linux):** not done. Check whether Chrome on v4l2loopback 0.12
  (Ubuntu 24.04 / Debian 12) lists and opens the camera, as part of the browser checks in
  [13 §5](13-progress-phase-l.md). Only if it doesn't: `VIDIOC_STREAMON` after `S_FMT` in
  `rtspcam-platform/src/linux/v4l2.rs` (ignore `EINVAL`/`ENOTTY`, like OBS) and `STREAMOFF`
  before closing. Tracked in issue #17.
- **Real apps through Frame Server:** the tests run the source in-process. Check with the DLL
  installed (`tools/vcam/install-dev.ps1`): the Windows Camera app, Teams and Chrome still open
  the camera and motion is smooth; an app that picks YUY2 or I420 (for example OBS's video
  capture device with the format set by hand, or `ffplay -f dshow -pixel_format yuyv422`) shows
  correct colours; and Frame Server's memory doesn't grow over a long run with several
  consumers (pooled samples).
