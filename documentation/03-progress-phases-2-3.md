# 03 — Progress: Phases 2 and 3

Status as of 2026-10-09. Continues [02-progress.md](02-progress.md) (Phase 1) against the plan in
[01-plan.md](01-plan.md). Phase 0 was skipped as a separate step: the parts of its spikes that
Phases 2–3 depend on were done as part of them (see §5).

## 1. Summary

| Phase | Status |
|---|---|
| 1 (leftover) | Test RTSP environment now **verified** (Docker). Two fixes, one known issue (MJPEG relay, §6). |
| 2 RTSP ingest and decode pipeline | **Done.** Exit criteria met; the 1-hour run showed a slight memory drift to keep an eye on (§4.2). |
| 3 Virtual camera media source | **Code done and tested in-process.** Not yet verified through Frame Server or in camera apps, which needs the one-time admin install (§4.3). The optional D3D11 path is deferred to Phase 8. |

## 2. Phase 2 checklist

| Plan item | Status | Where |
|---|---|---|
| `retina` session per camera, video stream selection, TCP/UDP, Basic/Digest | Done | `rtspcam-pipeline/src/source.rs` |
| Reconnect with exponential backoff and jitter; stall detection; status events | Done | `pipeline.rs`, `status.rs` |
| `Decoder` trait; `MfDecoder` (H.264/HEVC/MJPEG, sync MFT, NV12, stream change) | Done | `decode/mod.rs`, `decode/mf.rs` |
| `OpenH264Decoder` fallback | Done | `decode/openh264.rs` |
| `Scaler`: NV12 → NV12 (letterbox/crop/stretch), NV12 → BGRA | Done (CPU, bilinear) | `scale.rs` |
| `FrameBus` (latest frame only) | Done (`tokio::sync::watch`) | `bus.rs` |
| `rtspcam-cli probe` / `view` | Done | `rtspcam-cli/src/rtsp.rs` |
| Unit tests (decoder, scaler) and integration tests against mediamtx | Done | `tests/{source,decode,pipeline}.rs` |

Also added: paste-URL parsing in core (`parse_stream_url`, needed by the CLI now and the
Add/Edit dialog later), an animated test pattern with a machine-readable frame counter
(`pattern.rs`), and BMP snapshots/dumps in the CLI.

### How the pipeline works

```
 tokio task                          decode thread (owns the decoder / MF objects)
 RtspSource ──EncodedFrame──► bounded channel (8) ──► Decoder ──► FrameBus (latest Arc<Frame>)
     ▲  │                                               │
     │  └──── StreamState (watch) ◄──── events ◄────────┘
  backoff / stall detection
```

- The bounded channel pushes back on the network read when decoding is slow, instead of
  dropping compressed frames (which would corrupt the picture until the next key frame).
- The decoder waits for a key frame at session start and after any decode error.
- `StreamState`: `Idle`, `Connecting{attempt}`, `Streaming(StreamStats)`, `Retrying{error,
  retry_at, attempt}`. Stats are per second: size, fps, bitrate, receive-to-decode latency,
  totals, decoder name.
- Backoff 1 s doubling to 30 s with ±20% jitter, reset after a session that decoded pictures.
  Errors that retrying won't fix (bad credentials, no decoder, unsupported codec) wait the
  maximum straight away.
- Stall: reconnect after 5 s without a picture, or 15 s waiting for the first one (long GOPs).
- Errors carry an `ErrorKind` (Unauthorized, NotFound, Unreachable, Timeout, UnsupportedCodec,
  DecoderUnavailable, Stalled, ...) with a user hint, ready for the UI's friendly errors.
- Frames on the bus are at **source resolution**. Consumers scale to what they need (the camera
  client's negotiated format, the preview's window size), so one decode serves all of them.

## 3. Phase 3 checklist

| Plan item | Status | Where |
|---|---|---|
| Source/stream state machine (`Start`/`Stop`/`Pause`/`Shutdown`, `SetStreamState`, selection) | Done | `rtspcam-vcam/src/source.rs`, `stream.rs` |
| Media types NV12 + RGB32 × {1080p, 720p, 480p} × {30, 15} | Done (12 types) | `formats.rs` |
| Sample timing: `MFGetSystemTime`, fixed duration, repeat last frame | Done, paced by a delivery thread | `stream.rs` |
| IPC client; request the consumer's format; "No signal"/"not running" image | Done | `rtspcam-ipc`, `feed.rs`, `placeholder.rs` |
| Optional D3D11 path | **Deferred** (Phase 8) | |
| Panic safety around every COM entry point; logging | Done | `guard.rs`, `log.rs` |
| Self-registration (HKLM, `ThreadingModel=Both`) | Done | `registry.rs` |
| `rtspcam-vcam-mgr`: create (Drop = Shutdown), list, remove | Done | `rtspcam-vcam-mgr/src/lib.rs` |
| Test harness: in-process source + `IMFSourceReader` | Done | `rtspcam-vcam/tests/`, `rtspcam-cli vcam test` |
| `rtspcam-cli vcam add --name Test --pattern` | Done | `rtspcam-cli/src/vcam.rs` |

### IPC (`rtspcam-ipc`)

- One named pipe per camera, `\\.\pipe\rtspcam\<stream id>`, served by the app (and the CLI's
  `vcam add`). DACL: SYSTEM, LOCAL SERVICE (Frame Server) and the owner.
- Messages: 16-byte header (`RCAM`, version, kind, length) then Hello / SetFormat / Frame /
  Status / Goodbye. The DLL says which format the consumer picked; the server sends frames
  already scaled and converted to it, paced at its fps, plus a Status heartbeat every second.
- Everything the DLL reads is validated: body length capped before allocating, formats bounded,
  frame size must match the format exactly. A small random-input test exercises the parser.
- The DLL side is blocking std I/O on a helper thread (no tokio in the service); it reconnects
  when the app restarts and is cancelled with `CancelSynchronousIo`.

### The DLL (`rtspcam_vcam.dll`)

- `IClassFactory` → `IMFActivate` (forwards `IMFAttributes` to an inner store) →
  `IMFMediaSourceEx` + `IMFGetService` + `IKsControl` → one `IMFMediaStream2`.
- `RequestSample` only queues the request; a delivery thread answers at the negotiated rate with
  the newest app frame, so consumers that ask as fast as they can still get a steady 30/15 fps,
  and a stalled RTSP stream repeats its last frame for up to 3 s before showing "No signal".
- Placeholders: "RTSP Cam is not running", "Connecting...", "No signal" (with the app's error
  message), "Camera disabled", "This camera is not set up". Drawn with a built-in 5×7 font.
- Which stream a camera belongs to: the app attaches the stream id (and its configured output
  format, listed first) as device properties with `IMFVirtualCamera::AddProperty`; the source
  reads them back via the device's symbolic link (interface property, falling back to the device
  node). In-process tools set our own attribute instead. **Unverified with Frame Server** (§4.3);
  the source logs every attribute it receives if it can't find the id.
- Logs: `%ProgramData%\RtspCam\logs\vcam.log` (folder created by `DllRegisterServer` with write
  access for LOCAL SERVICE) and `OutputDebugString`; `RTSPCAM_VCAM_LOG=stderr` for tests.
- Objects and helper threads are counted; `DllCanUnloadNow` succeeds once all are gone.

## 4. Verification

All on Windows 11 with the test server in Docker (`bluenviron/mediamtx:latest-ffmpeg`).

### 4.1 Tests

- `cargo test --workspace`: all pass. With `RTSPCAM_TEST_SERVER=127.0.0.1:8554` the live tests
  run too (ingest of every stream, credentials and error classification, full pipelines with
  both H.264 decoders, 1080p, H.265, wrong password, prompt stop).
- clippy (`-D warnings`, workspace and the DLL's dependency set) and `cargo fmt --check`: clean.

### 4.2 Phase 2 exit criteria

| Criterion | Result |
|---|---|
| 4 streams at once | H.264 720p, H.264 1080p, H.265 720p and the credentialed stream, all 30.0 fps, release build. |
| Stable memory for 1 hour | Ran the full hour: ~108,000 frames per stream, 0 decode errors, 0 packets lost, no warnings or reconnects. Private bytes 108 MB at 1 min → 114 MB at 40 min → 117 MB at 60 min. Mostly flat, but a slow upward drift (~9 MB/h) that could be allocator fragmentation or a small leak. To recheck with a longer run (Phase 8 soak). |
| Recovers after the RTSP server restarts | `docker restart` mid-run: all four back within 1–2 s; H.265 took ~8 s (retina rejected joining mid-fragment, "FU has start bit unset", and retried). |
| < 150 ms pipeline latency | Receive-to-decode: 2–3 ms (H.264, MF), ~9 ms (OpenH264), ~14 ms (H.265). Network/server latency is not included. |

Dumped frames (`view --dump`) were checked by eye: correct colors and geometry for H.264, H.265
and the SMPTE bars of the secure stream.

### 4.3 Phase 3

Verified in-process (no registration needed):
- The source reader sees 12 formats, NV12 1280×720@30 first.
- Placeholder frames arrive at a steady 30 fps (30 frames in ~1 s), with increasing timestamps.
- Frames from a real pipe server arrive; switching to RGB32 640×480@15 works.
- After shutdown no object or helper thread is left and `DllCanUnloadNow` returns `S_OK`.
- The built DLL loaded through its `DllGetClassObject` export (`vcam test --pattern`):
  30.3 fps, test pattern frames received and decoded correctly (snapshot checked by eye).

**Not verified yet** (needs the admin install, then a person or `vcam read`):
- Registering in HKLM and Frame Server loading the DLL from `C:\Program Files\RtspCam`.
- That the camera id property reaches the source (S0.2). Fallback if not: a pool of CLSIDs.
- The camera in the Windows Camera app, Chrome, OBS and Discord (preview and remote side).
- The Phase 3 exit run: 1 hour in those apps, start/stop/reselect cycles without leaks in the
  Frame Server process.
- S0.6 (process exit removes the cameras, including a hard kill).

To run them:

```powershell
cargo build -p rtspcam-cli -p rtspcam-vcam
./tools/vcam/install-dev.ps1                  # asks for elevation (UAC)
cargo run -p rtspcam-cli -- vcam status
cargo run -p rtspcam-cli -- vcam add --name "RTSP Cam Test" --pattern
# in another terminal:
cargo run -p rtspcam-cli -- vcam list
cargo run -p rtspcam-cli -- vcam read "RTSP Cam Test" --pattern --seconds 10
```

## 5. Decisions and deviations

| Topic | Decision |
|---|---|
| Phase 0 | Not done as separate spikes. S0.4 (RTSP + decode) is covered by Phase 2; S0.1/S0.3/S0.7 are partly covered by Phase 3's in-process tests. S0.2 and S0.6 still need the Frame Server run. |
| IPC | Named pipes (plan default). Shared memory stays a Phase 8 option; local pipes carry 1080p NV12 at 30 fps easily. |
| Who scales for the camera | The app (pipe server), to exactly the consumer's format. The DLL only copies bytes. |
| Frame pacing | A delivery thread per running stream paces samples; `RequestSample` never blocks. |
| Format change in the DLL | Restarting the stream reconnects the pipe with the new format, instead of sending `SetFormat` on a pipe that is blocked in a read. The server still supports `SetFormat`. |
| Decoder threading | One decode thread per stream owns its Media Foundation objects. |
| MF output buffer | Reset the reused output buffer's length before each `ProcessOutput`. Without this the H.264 MFT fails every picture after the first ("CopyDecodedFrame failed"). |
| H.265 key frames | retina only flags IDR pictures as random access points, so open-GOP (CRA) streams never got parameter sets. The source detects any IRAP picture and prepends VPS/SPS/PPS itself. Worth reporting upstream. |
| `MFCreateVirtualCamera` | Resolved at run time from `mfsensorgroup.dll`, so the exe still starts (and can explain) on Windows 10. |
| Camera identity | Device properties via `AddProperty` (option a), unverified. |
| Errors | Classified by `ErrorKind` with hints, for the UI. |
| Developer install of the DLL | `tools/vcam/install-dev.ps1` until the Phase 7 installer exists. |

## 6. Known issues

- **MJPEG test stream:** mediamtx rejects the RTP/JPEG packets ffmpeg publishes ("received
  wrong fragment"), so `mjpeg-720p` never delivers frames. The MJPEG *decoder* is tested with a
  JPEG fixture instead; MJPEG streaming needs a real camera to test.
- **UDP tests in Docker:** Docker Desktop doesn't forward the server's RTP packets to the host;
  the UDP test is opt-in (`RTSPCAM_TEST_UDP=1`) for a native mediamtx.
- **retina strictness:** a single malformed fragment ends the session (H.265 after a server
  restart). It recovers through the normal reconnect.
- **Color:** BT.709 is assumed from 720p up and BT.601 below; full-range (MJPEG) input is not
  converted to video range yet.
- **CPU scaling:** fine for a few streams; the GPU path is Phase 8.

## 7. Next steps

1. Run the admin install and the Frame Server checks in §4.3 (camera id, Camera app, OBS,
   Discord, S0.6). Fix whatever they turn up, then do the 1-hour Phase 3 exit run.
2. Push and get the CI workflow green (still never run on GitHub).
3. Phase 4: the camera manager in `rtspcam.exe` (moves `BusSource` from the CLI into the app,
   on-demand pipelines, single instance, shutdown order and watchdog).
