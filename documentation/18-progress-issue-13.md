# 18 — Progress: 60 fps output and a "Source" resolution (issue #13)

Date: 2026-10-10. Continues [15 §5](15-obs-virtualcam-comparison.md#5-bigger-items-not-low-hanging)
("60 fps") and builds on the high-resolution pacing in [16](16-progress-issue-12.md). Tracked in
GitHub issue #13. Everything in the issue is built; §4 lists what still needs a person.

## 1. What was done

### 60 fps

- **Config** (`rtspcam-core/src/config/mod.rs`): `OutputFormat::FRAME_RATES` is
  `[60, 30, 15]`. Validation (`is_supported`) and the form use it, so nothing else changed there.
- **Form** (`rtspcam-engine/src/form.rs`): `FPS_LABELS` is "60 fps", "30 fps", "15 fps". A stream
  whose rate isn't in the list still opens at 30 fps (now index 1).
- **Windows advertised formats** (`rtspcam-vcam/src/formats.rs`): `RATES` is `[30, 15, 60]`
  (60 appended, so the order of the old types is unchanged and NV12 720p30 is still first).
  4 pixel formats × 3 rates × 3 sizes = **36 media types** (was 24); 48 with a "Source" size.
- **Pacing at 60 fps:** the high-resolution waitable timer from #12 holds 16.7 ms. The
  source-reader test now runs at 30 and 60 fps (`frame_gaps_are_even`,
  `frame_gaps_are_even_at_60_fps`); see §2. Timestamps step by 166 666 or 166 667 ticks.
- **Linux push backend** (`rtspcam-platform/src/push.rs`): unchanged. It paces with
  `Instant` deadlines of `1 s / fps` (exact to the nanosecond) and a condition-variable wait,
  which is sub-millisecond on Linux; its existing test already runs at 50 fps.
- **IPC limits** (`rtspcam-ipc/src/protocol.rs`): `MAX_FPS` stays 120, now with a compile-time
  check that it covers `FRAME_RATES`. `MAX_WIDTH`/`MAX_HEIGHT` are now taken from
  `OutputFormat::MAX_SIZE`, so the config's limit and the pipe's can't drift apart.
- **Repeated frames** (`rtspcam-engine/src/status.rs`, `rtspcam-app/src/ui/app.rs`): the stream
  details already showed the measured decoded rate; `frame_rate_note` adds a line when it is
  below 90 % of the output rate: "The camera sends 25.0 fps, fewer than the 60 fps output: apps
  see some pictures twice."

### "Source" resolution

- **Config:** `OutputFormat` has a new `source: bool`, written only when true
  (`"output": { "width": 1920, "height": 1080, "fps": 60, "source": true }`). No schema
  migration was needed: a missing field is `false`, so existing configs load and save exactly as
  before, and `CURRENT_VERSION` stays 1. An older build reading a new config ignores `source`
  and uses `width`/`height`, which the form sets to 1920×1080 (`OutputFormat::source(fps)`),
  a valid fixed size. With `source` set, any `width`/`height` validates; the rate must still be
  one of `FRAME_RATES`.
- **Form:** a fourth entry in the resolution list, "Source (the camera's own size)"
  (`SOURCE_RESOLUTION`).
- **Size rules** (`OutputFormat::source_size`): the camera's size made even (NV12, YUY2, I420
  need it) and, if larger than `MAX_SIZE` (4096×2160, the IPC limit), shrunk to fit with the same
  shape (4000×3000 → 2880×2160). Sizes under 2×2 are not offered.
- **Which option of the three in the issue:** the second and third together. The camera
  offers the source size *plus* the fixed list (so apps keep their usual choices), and the last
  seen source size is remembered across runs in `source-sizes.json` next to `config.json`
  (`rtspcam-engine/src/source_sizes.rs`, like OBS's `obs-virtualcam.txt`). Before the first
  connection it offers 1920×1080. When the stream connects with a size the camera doesn't
  offer, the camera is **re-created** with it, but only once no app is using the camera
  (re-creating under a call would take the device away from it; on Windows it shows up as a
  device change). Until then apps get pictures scaled to the size they picked, the existing
  behaviour.
- **Manager** (`rtspcam-engine/src/manager.rs`): `add_camera` passes the remembered size as
  `CameraSpec::preferred` (`offered_output`). A "Source" camera gets a small follower task
  (`follow_source_size`) that watches the stream's `StreamStats` size and the consumer count.
  It sends `Resize` messages to the reconciler, which now also handles them
  (`resize_camera`), so creating, re-creating and removing cameras stay one at a time. A
  message for a camera that was removed or rebuilt meanwhile is ignored. The size is
  remembered as soon as it is seen, even while an app holds the camera. Entries for streams that
  are gone or no longer "Source" are dropped on the next config apply. `CameraStatus::offered`
  carries the size offered, and the details read "Output: the camera's own size (1440x1080) @
  30 fps".
- **Mid-stream size change** (camera reconfigured): the same path. The follower sees the new
  size, apps keep their size (pictures scaled to it) until they let go, then the camera is
  re-created.
- **Windows** (`formats.rs`): a preferred size that isn't one of the three is added as a fourth
  size in every pixel format and rate, if it is even and within the IPC limits, and the
  preferred NV12 type goes first. The DLL already received the preferred format as a string
  attribute, so nothing else changed in the DLL or in `rtspcam-vcam-mgr`.
- **Hardware decoding:** nothing to change. Apps on a "Source" camera ask for the camera's
  size, so the decoder's size limit (14 §1, `source_size_for`) is at least the picture and
  `limited_size` leaves it whole: no GPU shrink. Tested in
  `the_cameras_own_size_is_decoded_without_shrinking`.
- **Linux:** the v4l2loopback device's format comes from `CameraSpec::preferred` as before, so
  it is the source size. A re-create goes through the existing "edited stream" path
  (`keep_device`), which sets the new format on the same device, or claims a new one if the
  device can't be re-formatted.

### Fix: "could not serve the camera's pipe (access is denied, os error 5)"

Found by hand on Windows with "Source": the re-create failed with this error, and the camera
was not re-made. The cause was the pipe, not the camera. `VcamBackend::create` restarted the
camera's pipe server by aborting the old task and serving the pipe again at once. `abort()`
only asks tokio to cancel, so the old pipe instances were still open a moment later, and
`NamedPipes::listen` asks for the *first* instance of the name (`first_pipe_instance`, which
keeps a second copy of the app from serving the same camera). Windows refuses that with
`ERROR_ACCESS_DENIED` while any old instance exists. Editing a stream had the same race, but
the pipeline stop before it gave the abort time to finish.

- `rtspcam_ipc::server::serve_until` stops on a signal: it cancels every client task, waits
  for them (`JoinSet::shutdown`) and drops the listener before returning.
  `rtspcam-platform/src/windows/camera.rs` keeps a `PipeServer` per camera and `stop_serving`
  signals it and waits (up to 2 s, on a plain channel so it works from any thread) until every
  pipe handle is closed.
- New `VirtualCameraBackend::reformat` for the "Source" re-create. It defaults to `create`;
  the Windows backend re-makes only the camera and leaves the pipe running, since the pipe
  carries whatever format the DLL asks for. Linux keeps the default (the pusher's format
  changes).
- Found on the way: a client whose task was cancelled (stopped server, or the old abort) never
  reported `client_disconnected`, so the camera's client count stayed too high. A drop guard in
  `serve_client` now reports it however the connection ends.
- Test: `rtspcam-platform/tests/transport.rs::an_endpoint_can_be_served_again_right_after_stopping`
  serves, connects a client, stops and serves again three times, with each client still holding
  its end. A client's open handle does not block the new first instance once the server's
  handles are closed.

### Test server

`tools/test-rtsp/mediamtx.yml` has two new on-demand streams: `h264-1080p60` (1920×1080 60 fps)
for the bench, and `h264-4x3` (1440×1080 25 fps), a size no camera offers, for "Source". Each
read path also needs a `read` permission entry in `authInternalUsers`; without it mediamtx
closes the connection after DESCRIBE with no log line at `info`.

## 2. Measurements

Same machine as [14 §3](14-progress-phase-8.md#3-measurements) (Ryzen 9 5950X, RTX 3080 Ti,
Windows 11 Pro 26200), test server in Docker, `cargo build --release`, one consumer reading
1920×1080, 4 s warm-up, 15 s measured, one run each. CPU is process CPU of `rtspcam-cli bench`
in % of **one** core.

| Source → consumer | Software decoding | Hardware decoding |
|---|---|---|
| 1080p30 → NV12 @ 30 | 19.0 | 4.0 |
| 1080p60 → NV12 @ 60 | 48.3 | 14.3 |
| 1080p30 → RGB32 @ 30 over the pipe | 45.0 | 32.5 |
| 1080p60 → RGB32 @ 60 over the pipe | 113.0 | 70.9 |
| 1080p30 → NV12 @ 60 | 20.5 | 6.8 |

Every 60 fps run delivered 59.5–60.1 new pictures per second to the consumer. The last row is a
30 fps camera behind a 60 fps output: the consumer still gets 30 new pictures a second (the
DLL repeats each one), and the cost is that of 30 fps.

Per stage at 60 fps, ms per picture: decode 2.6–2.8 (software) or 6.8–7.0 (hardware, of which
6.4–6.5 is the copy-back wait), scale 0.2–0.6 (a copy at the same size), NV12 → BGRA 5.8,
pipe write 2.4–2.6. Per picture these match 30 fps; CPU doubles with the rate, except that
hardware NV12 went from 4.0 to 14.3 % (the copy-back's waiting thread counts toward process
CPU). RGB32 at 1080p60 needs more than one core in software: an app that picks RGB32 at
1080p60 is the expensive case, and the NV12/YUY2 types listed before it are the cheap way.

Frame gaps through `IMFSourceReader` (in-process, 720p NV12, `cargo test -p rtspcam-vcam --test
source_reader frame_gaps -- --nocapture`):

| Rate | Frames | Mean gap | Min | Max | Within 2 ms of the interval |
|---|---|---|---|---|---|
| 30 fps | 89 | 33.33 ms | 32.94 ms | 33.83 ms | 89 of 89 |
| 60 fps | 179 | 16.67 ms | 15.97 ms | 17.46 ms | 179 of 179 |

## 3. Tests added

- `rtspcam-core`: `source_output_round_trips_and_is_left_out_when_off`, `supported_outputs`,
  `source_sizes_are_even_and_within_the_limits`.
- `rtspcam-engine`: `source_resolution_and_60_fps_round_trip` (form), `output_lines` and
  `repeated_pictures_are_pointed_out` (status), `source_sizes::*`,
  `the_cameras_own_size_is_decoded_without_shrinking`,
  `source_streams_offer_the_size_seen_or_their_own`, and in `tests/manager.rs`
  `source_streams_offer_the_size_seen_last_time` and (with the test server)
  `a_source_stream_offers_the_cameras_size_once_no_app_uses_it`, which checks that the camera
  is not re-created while a consumer reads it, is re-created once it leaves, and then serves
  unscaled 1440×1080 pictures.
- `rtspcam-vcam`: `thirty_six_formats_nv12_720p30_first`,
  `the_source_size_is_offered_besides_the_usual_ones`, and through `IMFSourceReader`
  `offers_thirty_six_formats_nv12_720p30_first`, `offers_a_source_size_first_besides_the_usual_ones`
  (2560×1440 read in YUY2) and `frame_gaps_are_even_at_60_fps`.

## 4. Not verified (needs a person)

- **Real apps at 60 fps:** with the DLL installed, check that the Camera app, OBS and Chrome
  list the 60 fps types and play them smoothly, and that apps which pick "the highest rate"
  don't now pick 60 fps on cameras that send 25–30 (they'd see repeated pictures; the stream
  details say so).
- **The re-create as apps see it:** after a "Source" camera is re-created, an app that was
  closed and opened again lists the new size. Apps that cache device formats (some browsers)
  may need a restart.
- **Linux:** a "Source" camera on v4l2loopback 0.12 and 0.13+, including a re-create with
  another size while a reader is closed, and 60 fps in a browser.
- **Size limit on the GPU with a clamped source:** a camera larger than 4096×2160 (8K) is offered
  shrunk; not tried with a real one.
