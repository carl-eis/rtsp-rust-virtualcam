# RTSP Cam

A Windows 11 desktop app, written in Rust, that turns RTSP streams from IP cameras, NVRs or
`mediamtx` into **webcams**. Each stream shows up as its own camera (for example
"Front Door – Windows Virtual Camera") in Discord, Teams, Zoom, OBS, Chrome/Edge and the
Windows Camera app.

It uses the Windows 11 Media Foundation virtual camera API (`MFCreateVirtualCamera`), so no
kernel driver is needed. Cameras exist only while the app is running.

> **Status:** early development. Phase 1 (workspace, config, logging, CI, test environment)
> is in place. There is no virtual camera or UI yet. See the
> [implementation plan](documentation/01-plan.md).

## Requirements

- Windows 11 (build 22000 or later)
- [Rust](https://rustup.rs/). The toolchain is pinned in `rust-toolchain.toml`, and rustup
  installs it automatically.
- **Visual Studio Build Tools** with the *Desktop development with C++* workload (MSVC
  linker and Windows SDK):

  ```powershell
  winget install Microsoft.VisualStudio.2022.BuildTools --override "--quiet --wait --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended"
  ```

- Optional: [mise](https://mise.jdx.dev/) for the task shortcuts below.
- Optional: `mediamtx` + `ffmpeg`, or Docker, for the [test RTSP server](tools/test-rtsp/README.md).

## Build and test

```powershell
cargo build --workspace
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all
```

Or with mise: `mise run test`, `mise run lint`, `mise run ci`.

> If you build from Git Bash, its `/usr/bin/link` can shadow MSVC's `link.exe`. Build
> from PowerShell or a *Developer PowerShell for VS* prompt instead.

## Layout

```
crates/
  rtspcam-core/       config model (JSON), Protocol + URL builder, DPAPI secrets, validation,
                      atomic save, migrations, file watching, logging, shared constants
  rtspcam-ipc/        app ↔ virtual camera frame protocol          (Phase 3)
  rtspcam-pipeline/   RTSP ingest, decoding, scaling, frame bus    (Phase 2)
  rtspcam-vcam/       rtspcam_vcam.dll: COM media source           (Phase 3)
  rtspcam-vcam-mgr/   safe wrapper over MFCreateVirtualCamera      (Phase 3)
  rtspcam-app/        rtspcam.exe: the desktop app                 (Phases 4–5)
  rtspcam-cli/        rtspcam-cli.exe: developer tool
tools/test-rtsp/      mediamtx + ffmpeg test streams
documentation/        plan and decision records
```

## Configuration

Everything lives in one JSON file at `%APPDATA%\RtspCam\config.json`:

```json
{
  "version": 1,
  "settings": { "start_with_windows": false, "minimize_to_tray": true, "log_level": "info" },
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

- Every field is optional and has a default. Unknown fields are kept when the file is saved.
- Passwords are encrypted with Windows DPAPI (current user only). A plain-text password typed
  into the file by hand is encrypted the next time the app saves.
- Saves are atomic, and the previous version is kept as `config.json.bak`.
- Edits made to the file while the app runs are picked up automatically.

The developer CLI can inspect it:

```powershell
cargo run -p rtspcam-cli -- config path
cargo run -p rtspcam-cli -- config show       # passwords redacted
cargo run -p rtspcam-cli -- config validate
cargo run -p rtspcam-cli -- config add-test-streams
```

## Logs

Logs are written to `%LOCALAPPDATA%\RtspCam\logs\` (rotated daily, 7 files kept). Set
`RTSPCAM_LOG` to override the level, for example `RTSPCAM_LOG=debug`.
