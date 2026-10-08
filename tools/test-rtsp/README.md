# Test RTSP server

A local RTSP server ([mediamtx](https://github.com/bluenviron/mediamtx)) that serves ffmpeg
test-pattern streams, so you can develop without a real IP camera.

| URL | Codec | Size / rate | Auth |
|---|---|---|---|
| `rtsp://127.0.0.1:8554/h264-720p` | H.264 Main | 1280×720 @ 30 | none |
| `rtsp://127.0.0.1:8554/h264-1080p` | H.264 High | 1920×1080 @ 30 | none |
| `rtsp://127.0.0.1:8554/h265-720p` | H.265 | 1280×720 @ 30 | none |
| `rtsp://127.0.0.1:8554/mjpeg-720p` | MJPEG (RTP/JPEG) | 1280×720 @ 15 | none (see known issue) |
| `rtsp://127.0.0.1:8554/secure` | H.264 | 1280×720 @ 30 | `rtspcam` / `rtspcam-test` |

Port 8554 is used instead of 554 so nothing needs admin rights or clashes with a real server.

> **Known issue (MJPEG):** mediamtx rejects the RTP/JPEG packets ffmpeg publishes
> ("received wrong fragment" in its log), so `mjpeg-720p` answers DESCRIBE but never delivers
> frames. `probe.ps1` only checks the SDP, so it still reports the stream as OK. Both
> sides' fragment-offset logic looks correct and the cause hasn't been found yet. The MJPEG decoder
> is covered by a JPEG fixture instead; test MJPEG streaming against a real camera.

## Run it

Natively (needs `mediamtx` and `ffmpeg` on `PATH`, or copied into `tools/test-rtsp/bin/`):

```powershell
scoop install ffmpeg mediamtx   # or download them yourself
./tools/test-rtsp/start.ps1     # or: mise run rtsp
```

With Docker Desktop instead (image `bluenviron/mediamtx:latest-ffmpeg`):

```powershell
./tools/test-rtsp/start.ps1 -Docker           # foreground, Ctrl+C to stop
./tools/test-rtsp/start.ps1 -Docker -Detach   # background; stop with: docker stop rtspcam-test-rtsp
```

## Check it

```powershell
./tools/test-rtsp/probe.ps1           # or: mise run rtsp-probe
./tools/test-rtsp/probe.ps1 -Docker   # uses the ffprobe inside the test container
```

This runs `ffprobe` against every stream, checks the codec, and checks that `secure` refuses
clients without credentials. You can also open a stream in VLC or `ffplay`.

## Use it in RTSP Cam

Add all test streams to your config:

```powershell
cargo run -p rtspcam-cli -- config add-test-streams
cargo run -p rtspcam-cli -- config show
```

Use `--host <ip>` to point at mediamtx on another machine.
