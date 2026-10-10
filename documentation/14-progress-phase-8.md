# 14 — Progress: Phase 8 performance (steps 8.0–8.3 of 08.1)

Status (2026-10-10): steps 8.0, 8.1 (with the portable P8-11 change), 8.1a, 8.2 and the
parts of 8.3 the measurements called for are built on `feat/performance`, following
[08.1](08.1-Plan-Phase-8.md). 8.4 (GPU textures into the virtual camera) stays deferred: the
target is met without it (§3). The rest of Phase 8 (soak tests, compatibility matrix, fuzzing,
crash reports; 08.1 §7) has not started.

| Step (08.1 §4) | State |
|---|---|
| 8.0 Measure | done: stage timers, process CPU on every OS, `rtspcam-cli bench`, the tables in §3 |
| P8-11 Decoder swap without reconnect | done, tested with a fake decoder |
| 8.1 D3D11 hardware decoding | done; tested on an NVIDIA RTX 3080 Ti (§4) |
| 8.1a "Use hardware decoding" setting | done; the dialog needs a look by a person (§5) |
| 8.2 Smaller frames earlier | done, with the Video Processor on the decoder's texture |
| 8.3 Faster CPU stages | scaler and per-format cache done; the rest not needed now (§6) |
| 8.4 GPU frames into the camera | deferred, as planned (P8-6) |

**Result:** a 1080p H.264 camera read by one 720p app costs 5–6 % of one CPU core with
hardware decoding, against 29 % before this work; a 1080p H.265 camera 4–5 % against 91 %, a 4K
camera 5 % against 69 %. That is 0.2 % of this 32-thread machine, and under 1 % of an
8-thread machine of similar speed, so P8-7's "under 3 % per 1080p stream on a mid-range
machine" is met. Before, the same stream was about 3.6 % of such a machine.

## 1. What was done

### 8.0 Measuring (`rtspcam-core`, `rtspcam-platform`, `rtspcam-cli`)

- `rtspcam_core::profile`: process-wide time per stage (`Stage::Decode`, `CopyBack`,
  `GpuScale`, `Adjust`, `Scale`, `Convert`, `Deliver`), two relaxed atomic adds per call, so it
  is always on. `profile::take()` reads and resets. Timed in: the decode loop (whole
  `Decoder::decode`, so it includes the copy-back), the copy-back and GPU scaling in
  `windows/gpu.rs`, `Adjusted::adjust` (crop/rotate/flip/text), `CameraSource` (scale,
  convert), the pipe write in `rtspcam_ipc::server` and the device write in `push::Pusher`.
  The times are wall-clock time on the working thread: CPU time for stages that never wait,
  waiting included for `CopyBack` (the GPU) and `Deliver` (the reader).
- `StreamStats::decode_time`: average decoder time per picture over the last second.
- `desktop::process_cpu_time()`: `GetProcessTimes` on Windows, `getrusage` on Linux and macOS
  (`libc` moved from the Linux-only to the Unix dependencies).
- `rtspcam-cli bench <urls…>` runs the app's `CameraManager` with a backend whose cameras are
  reader threads in the same process (`--pipe`: through the real frame transport, so the pipe
  write and read are in the numbers), waits for every stream, measures for `--seconds`, and
  prints Markdown tables: per stream (decoder, decoded size, fps, decode time), per stage, and
  process CPU. Options: `--consumers`, `--size`, `--fps`, `--pixel nv12|rgb32|yuyv|i420`,
  `--hardware on|off`, `--limit on|off` (8.2), `--rotate`, `--crop`, `--show-name`.
- `probe` and `view` take `--hardware on|off` (default off, P8-12); `view` prints process CPU,
  the stage times and the decode time once a second.
- The test server has two more streams, started only while read: `h265-1080p` and
  `h264-2160p` (`tools/test-rtsp/mediamtx.yml`).

### P8-11 Replacing a decoder without reconnecting (`rtspcam-pipeline`)

- `ErrorKind::DecoderLost` (`error.rs`).
- `decode/mod.rs`: `set_use_hardware(on)` (also tells the platform decoders), `uses_hardware()`,
  and `generation()`, bumped when the switch changes. Hardware decoding is off in a process
  until someone turns it on (P8-12): the engine does it from the config, tests and the CLI
  only when asked.
- `pipeline.rs`: the decode thread is now a `DecodeLoop` with a decoder factory (tests pass a
  fake one). It replaces the decoder
  - on `DecoderLost`: dropped at once, a new one at the next key frame. If the lost decoder
    never produced a picture (hardware decoding doesn't work for this stream), or after three
    losses in a session, the rest of the session decodes in software (`create_decoder_with(..,
    hardware: false)`).
  - when `generation()` changed: the old decoder keeps going until the next key frame, then the
    new one takes over, so the picture never stops.
  - It also reports the decoder's name again when it changes (a hardware decoder adds
    `, D3D11` once its first picture came from the GPU), so the stream details are right.
- Tests: `pipeline::tests` (lost before / after the first picture, renames and size limits,
  switching hardware at a key frame).

### 8.1 D3D11 hardware decoding (`rtspcam-platform/src/windows/`)

- `gpu.rs` (new):
  - `Gpu::probe()`: the default DXGI adapter (`EnumAdapters1(0)`); a software adapter is
    "No graphics card with video decoding found"; a D3D11 device with video support and
    multithread protection; the decoder profiles (`H264_VLD_NOFGT` → H.264, `HEVC_VLD_MAIN` →
    H.265); an `IMFDXGIDeviceManager`. Every failure is a sentence for the user.
  - `Hardware`: the switch, and the probe's result. The probe runs on a background thread from
    `install()`; a decoder created while it runs waits up to 2 s, then uses software. A lost
    device is forgotten (`lost()`), and the next decoder probes again on its own thread.
  - `CopyBack`: per decoder. Copies the visible part of the decoder's texture slice into our own
    staging texture (`CopySubresourceRegion` with a box, so the 1088th row isn't copied),
    maps it and builds a `Frame` from it. With a size limit (8.2) it first scales with the
    Video Processor. It waits for the GPU on a D3D11 fence and an event, **outside** the device
    lock (§2).
- `mf_decoder.rs`: `MfDecoders` holds the `Hardware`. `create(.., hardware)` gets the device
  when hardware decoding is on and the device decodes the codec (MJPEG never), and falls back to
  software if the MFT then fails to start. `MfDecoder::new` sends
  `MFT_MESSAGE_SET_D3D_MANAGER` when the MFT is `MF_SA_D3D11_AWARE`, before `SetInputType`.
  `convert()` checks every output sample: an `IMFDXGIBuffer` with an NV12 texture goes through
  `CopyBack`, anything else through the old `ConvertToContiguousBuffer` path. The name ends in
  `, D3D11` while pictures come from the GPU. A failure in D3D11 mode becomes `DecoderLost`
  when the device was removed or before the first picture.
- The output-buffer reset from [04 §3](04-session-2026-10-09.md#3-lessons-learned-the-non-obvious-parts)
  only applies to samples we allocate, as before; in D3D11 mode the MFT allocates them. The HEVC
  extension MFT works in D3D11 mode too (`Media Foundation (HEVCVideoExtension, D3D11)`).

### 8.1a The setting (`rtspcam-core`, `rtspcam-engine`, `rtspcam-app`)

- `AppSettings::hardware_decoding`, default `true` (old files get it from `Default`; no
  migration).
- `decode::HardwareDecoding` (`Checking`, `Available { device, codecs }`, `Unavailable(why)`)
  with `describe()`, `PlatformDecoders::hardware()` / `set_use_hardware()` with defaults for
  Linux and macOS ("Not available on this system yet"), and the free functions
  `hardware_decoding()` / `set_use_hardware()`.
- `CameraManager::apply` calls `set_use_hardware(settings.hardware_decoding)` before handing the
  config on, so it runs at startup (before any stream starts), after the Settings sheet, and
  after a hand edit picked up by the file watcher; running streams switch at their next key
  frame.
- Settings sheet: **Use hardware decoding** with a line under it: `Device: NVIDIA GeForce RTX
  3080 Ti (H.264, H.265)`, greyed while unchecked; disabled with the reason when unavailable
  (shown unchecked, but saving keeps the stored value); `Checking…` while the probe runs,
  filled in by a 250 ms timer.

### 8.2 Smaller frames earlier

- `Decoder::limit_size(Option<(w, h)>)`, default no-op. The box means "cover": the decoder
  scales by `min(1, max(w / width, h / height))`, keeping the shape
  (`decode::limited_size`, integer maths, even sizes).
- `Pipeline::set_size_limit` passes it to the decode thread through a shared atomic; the decode
  loop hands it to the decoder when it changes and to every new decoder.
- The engine records what is asked for instead of changing `FrameSource`: every
  `CameraSource::next_frame` and the preview's `Preview::latest_for(w, h)` note the size (and
  fit mode) in `CameraShared::note_request`; sizes not asked for in 3 s are forgotten. The
  limit is the largest box over those, mapped back through rotation and crop. For letterboxed
  consumers (and the preview) it uses the camera's shape, once a picture arrived, to ask for
  exactly the scale that fits, so a 1080p camera turned upright for a 1280×720 app is decoded
  at 720×406 instead of 1920×1080.
- `Frame::source_size`: the size the camera sent. The colour matrix guess
  (`Matrix::for_frame`) uses it, so a 1080p picture shrunk to 360 rows stays BT.709.
- Windows: `CopyBack` scales on the GPU with `ID3D11VideoContext::VideoProcessorBlt` straight
  from the decoder's texture slice into an NV12 render target (one input view per decoder
  surface, cached), same colour space on both sides so it only scales. Software decoders ignore
  the limit.
- `ManagerOptions::limit_decoded_size` (default on) turns it off, for measuring.

### 8.3 Faster CPU stages (`rtspcam-pipeline`, `rtspcam-engine`)

- The scaler is separable: each output row first blends its two source rows into a `u16`
  row (a plain loop the compiler vectorizes), then each output sample blends two values of it.
  Bit-exact with the old code, which is kept in the tests as the reference
  (`matches_the_reference_scaler_exactly`, random pictures, five geometries, three fit modes).
- `CameraSource` keeps one scaler and the last result per output format
  (`(width, height, pixel format)`): a second app reading the same format gets a copy, apps
  reading different formats don't wait for each other or make the scaler start over, and the
  non-NV12 formats no longer allocate a scaled `Frame` per call. Formats not asked for in 10 s
  are dropped.

### Late pictures (`rtspcam-ipc`)

With hardware decoding the decoded pictures arrive with more timing jitter (they wait for the
GPU), and the pipe server's fixed frame slots turned a picture a few milliseconds late into a
dropped frame: apps got 25–26 fps from a 30 fps camera. A slot without a new picture now checks
again every 3 ms for up to half a slot while the stream is streaming. 30 fps reach the reader
again (§3). The bench's direct reader does the same. The Linux `Pusher` was not changed (no
hardware decoding there yet; §6).

## 2. Decisions beyond the plan

| Decision | Why |
|---|---|
| Our own staging texture from the start, no `IMF2DBuffer::Lock2D` | GPU scaling (8.2) needs our own textures anyway, and the box copy skips the padding rows. |
| Wait for the GPU on an `ID3D11Fence` with an event, outside the device lock, then `Map` | The first version used a blocking `Map` under the lock: this NVIDIA driver busy-waits in it: process CPU stayed at 25 % of a core (no gain), and all streams would queue behind each other's waits. With the fence, in the same run setup: 3.7 %. Without fence support (old drivers) it falls back to the blocking `Map`. |
| `limit_size` means "cover", and the engine computes the box | Correct for every fit mode without the decoder knowing about fit modes; the engine makes it exact for letterbox. |
| Sizes are learnt from `next_frame` calls, not from `client_connected` | `client_disconnected` doesn't say which format left, and `SetFormat` changes formats mid-connection; the calls see both, and Linux's `Pusher` too. No trait change. |
| Device-lost handling re-probes on the decode thread of the next decoder | Keeps one device, and the adapter shown in Settings is the one decoding (P8-10). |
| Hardware tests in their own test binary (`tests/windows_decode_hardware.rs`) | The switch is process-wide; the software tests must stay in software. |
| 8.3 "SIMD" by restructuring for auto-vectorization, not intrinsics | No `unsafe`, same code on every OS and CPU, exact results. |
| Late-picture polling in the pipe server | Needed for the hardware path to deliver full frame rate; cheap (a few `next_frame` calls per slot, only while streaming). |

## 3. Measurements

Machine: AMD Ryzen 9 5950X (16 cores, 32 threads), NVIDIA GeForce RTX 3080 Ti (driver
32.0.16.1742), Windows 11 Pro 26200. Test server:
mediamtx with ffmpeg test patterns in Docker on the same machine (`tools/test-rtsp`). Each
stream read by one consumer at 1280×720 NV12 30 fps unless noted, 4 s warm-up, 15 s measured,
`cargo build --release`. "CPU" is process CPU of the bench in % of **one** core.
"Before" is the code at the start of this work (`a4e227d` plus the stage timers); "Software"
and "Hardware" are the finished code with hardware decoding off and on.

Process CPU per stream, % of one core (two runs each for the finished code; software decoding
varies a lot between runs because the Media Foundation software decoders run their own thread
pools):

| Streams, consumer | Before | Software now | Hardware now |
|---|---|---|---|
| 720p H.264 | 15.7 | 12.1–19.1 | 4.6–5.2 |
| 1080p H.264 | 28.7 | 26.1–32.5 | 5.1–6.0 |
| 1080p H.265 | 90.7 | 84.0–110.6 | 4.2–4.7 |
| 2160p (4K) H.264 | 69.1 | 67.2–81.4 | 4.7–5.1 |
| 4 × 1080p H.264 | 49.2 | 48.4–53.5 | 4.5–6.5 |
| 8 mixed (2 each of 720p/1080p H.264/H.265) | 75.5 | 56.2–62.8 | 6.0–6.2 |
| 1080p H.264, RGB32 consumer over the pipe | 44.7 | 36.4–40.6 | 18.5 |
| 1080p H.264, YUYV consumer over the pipe | 36.4 | 29.7–33.9 | 8.5–9.9 |
| 1080p H.264, two NV12 consumers | 45.4 | 27.7–28.8 | 4.0–5.2 |
| 1080p H.264, rotated 90° with the name drawn | 38.7 | 35.4–37.7 | 4.7–7.3 |

What each step contributed, hardware decoding with the size limit off (`--limit off`: 8.1
alone, frames copied back at full size and scaled on the CPU; before the 8.3 scaler):

| Stream | 8.1 alone | 8.1 + 8.2 |
|---|---|---|
| 720p H.264 | 3.7 | 4.6–5.2 |
| 1080p H.264 | 14.4 | 5.1–6.0 |
| 1080p H.265 | 15.2 | 4.2–4.7 |
| 2160p H.264 | 20.6 | 4.7–5.1 |
| 4 × 1080p H.264, per stream | 14.2 | 4.5–6.5 |

(720p into a 720p camera needs no shrinking; the difference there is run-to-run noise.)

Every run delivered 29.5–30.1 fps to each consumer, except one hardware YUYV-over-the-pipe run
at 28.3 (the other: 29.7). Before the late-picture change (§1) hardware runs delivered 25–26
fps over the pipe and with 4K sources.

Per stage, 1080p H.264 → one 1280×720 NV12 consumer, ms per picture:

| Stage | Before | Software now | Hardware now |
|---|---|---|---|
| Decoder thread (`decode`, includes the copy-back) | 2.55 | 2.54 | 7.75 |
| of which copy-back (mostly waiting for the GPU) | — | — | 7.43 |
| Scale to 1280×720 | 3.04 | 1.93 | 0.12 (a row copy: the GPU already made 1280×720) |
| Rotate 90° + name (`adjust`, the rotation run) | 4.62 | 3.94–4.40 | 0.60–0.80 (decoded at 720×406) |
| NV12 → BGRA (RGB32 run) | 2.45 | 2.35 | 2.50 |
| Pipe write (RGB32 run, includes waiting for the reader) | 1.05 | 1.02–1.10 | 1.02–1.10 |

Raw outputs of every run are reproducible with the commands in §7; the tables above were
collected with a script running the bench over the cases listed, 15 s each.

What the numbers said, and what was done about it (P8-1):

- Before, CPU **scaling cost as much as decoding** at 1080p (3.0 ms vs 2.6 ms of decoder
  thread time, plus the MF decoder's own worker threads), and twice that with two consumers.
  → 8.2 (shrink on the GPU) and 8.3 (faster scaler, one scaling per format).
- H.265 in software is by far the most expensive (91 % of a core for one 1080p stream).
  → 8.1.
- The pipe write is cheap (about 1 ms per 720p RGB32 picture, mostly waiting for the reader).
  → shared-memory IPC (08.1 §7) is not needed.
- RGB32 consumers pay 2.5 ms per 720p picture for NV12 → BGRA. Left as is (§6).
- The copy-back's 7–20 ms per picture is waiting for the GPU (the decode itself), not CPU;
  it adds that much latency compared with software decoding of small streams.

## 4. Verified

- `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings` and
  `cargo test --workspace` on Windows, also with `RTSPCAM_TEST_SERVER` (mediamtx in Docker).
  On Linux (Docker, `rust:1-bookworm`, the command in [10 §7](10-progress-cross-platform.md#7-step-e-ci)):
  clippy with `-D warnings` and `cargo test --workspace` pass. The OS-`cfg` check passes
  (run with `git grep --untracked` on the host: inside the container `git` can't read a
  worktree, so the script passes without checking anything there).
- `tests/windows_decode_hardware.rs` on the RTX 3080 Ti: H.264 decoded on the GPU at full size
  (name ends in `, D3D11)`, picture content checked), shrunk on the GPU to 160×120 with the
  content and `source_size` checked, and a software decoder on request ignoring the limit.
  On a machine without hardware decoding the three tests pass with a "skipped" message.
- The bench with hardware decoding on, through the pipe in RGB32 and YUYV, with two consumers,
  with rotation and the name overlay, and with 8 mixed H.264/H.265 streams: every stream
  `D3D11`, 30 fps decoded and delivered.

## 5. Not verified (needs a person)

- The Settings sheet: the checkbox, the device line, the disabled state and `Checking…`. It
  compiles and the logic is small, but nobody has looked at it.
- Turning the checkbox off and on while a camera is in use: the decoder should switch at the
  next key frame, with no placeholder and no reconnect (the unit test covers the decode loop,
  not the whole app).
- A GPU-less VM (or the GitHub runner): the probe should say "No graphics card with video
  decoding found", the checkbox be disabled, and decoding be software.
- Device loss: a driver update or a GPU reset (TDR) while streaming. The code returns
  `DecoderLost`, forgets the device and probes again; never triggered here.
- Other GPUs: Intel and AMD drivers, hybrid laptops (the adapter shown must be the one
  Windows' graphics preference picks), and older drivers without D3D11 fences.
- Real cameras in real apps with hardware decoding on (Discord, Teams, OBS, the Camera app):
  colours (Video Processor colour space), odd sizes, and the latency the GPU wait adds.
- Long runs with hardware decoding (GPU memory, the cached input views) — part of the soak
  tests in 08.1 §7.
- macOS: not built here. The only new macOS code is `process_cpu_time` (`getrusage`, shared
  with Linux through `unix/`, `libc` now a Unix dependency); CI builds it.

## 6. Known gaps and what was left out

- **8.4** (GPU textures into the virtual camera): not needed for the target; still deferred.
- **NV12 → BGRA** is still scalar (2.5 ms per 720p picture, RGB32 consumers only; most apps take
  NV12). A candidate if RGB32 consumers turn out to be common.
- **`feed.rs` buffer pool in the DLL**: the allocation per frame inside Frame Server could not be
  measured with the bench (it runs in Frame Server, not in the app). Not changed; measure it
  with a real consumer before touching the riskiest process.
- **Linux `Pusher`** writes in fixed slots without the late-picture check; not needed until
  Linux gets a hardware decoder (VA-API), whose plan (08.1 §5) is unchanged.
- The **latency** of hardware decoding is higher than software for small streams (the copy-back
  waits for the GPU: about 4 ms at 720p, 7 ms at 1080p, 19 ms at 4K on this machine).
- 10-bit H.265 (P010) is still unsupported on both paths (08.1 8.1).

## 7. How to resume

- Re-measure: start the test server (`./tools/test-rtsp/start.ps1 -Docker -Detach`), then
  `cargo run --release -p rtspcam-cli -- bench rtsp://127.0.0.1:8554/h264-1080p --hardware on`
  (add `--hardware off`, `--limit off`, `--consumers 2`, `--pixel rgb32 --pipe`, more URLs).
- GPU tests: `cargo test -p rtspcam-platform --test windows_decode_hardware -- --nocapture`
  on a machine with a graphics card.
- Next in Phase 8: the person checks in §5, then 08.1 §7 (soak, compatibility matrix, fuzzing,
  crash reports).
