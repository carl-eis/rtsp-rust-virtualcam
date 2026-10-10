# RTSP Cam

Use your IP cameras as webcams. RTSP Cam takes RTSP streams from IP cameras, NVRs or
`mediamtx` and turns each one into a camera (for example "Front Door") that Discord, Teams,
Zoom, OBS, browsers and the Windows Camera app can pick like any webcam.

**Why:** call and streaming apps only list webcams, not network cameras. RTSP Cam bridges the
two without a kernel driver on Windows 11 (it uses the Media Foundation virtual camera API), and
the cameras exist only while the app is running.

![RTSP Cam showing a live camera preview and its status](screenshots/main-window.png)

## Quickstart

**1. Install.** Download the latest package from
[Releases](https://github.com/carl-eis/rtsp-rust-virtualcam/releases):

| OS | Package | Install |
|---|---|---|
| Windows 11 | `RtspCam-<version>-setup.exe` | Run it (asks for admin once). |
| Debian 12+, Ubuntu 22.04+ | `rtspcam_<version>-1_amd64.deb` | `sudo apt install ./rtspcam_<version>-1_amd64.deb` |
| Fedora | `rtspcam-<version>-1.x86_64.rpm` | Enable [RPM Fusion](https://rpmfusion.org/), `sudo dnf install akmod-v4l2loopback`, then `sudo dnf install ./rtspcam-<version>-1.x86_64.rpm` |

macOS has no package yet; [build from source](#building-from-source) (streams preview, but
don't become cameras yet).

**2. Add a camera.** Start RTSP Cam and click **Add stream**. Enter the camera's IP address,
user name and password (the **Camera brand** box fills in the port and path for common brands),
click **Test connection**, then **OK**. The live preview appears in the window.

Or click **Find cameras** to list the ONVIF cameras on your network, pick one, enter its login,
click **Get streams** and choose the main or sub stream.

![The Find cameras dialog listing two cameras](screenshots/find-cameras.png)

**3. Use it.** Pick the camera by its stream name in Discord, OBS, your browser, etc. Restart an
app that was already open so it sees the new camera.

## Status

Early development; version 1.2.1.

| | Windows 11 | Linux | macOS |
|---|---|---|---|
| App, stream list, live preview, dialogs, tray | yes | yes (tray needs a StatusNotifierItem host) | builds and passes its tests in CI; **not yet run** |
| Virtual cameras | yes (checked through Frame Server; not yet confirmed in call apps) | yes, with [v4l2loopback](#virtual-cameras-on-linux); checked with ffmpeg and v4l2-ctl, not yet in browsers or call apps | not yet |
| Decoding | H.264, H.265 (with the HEVC extension), MJPEG | H.264 only (OpenH264) | H.264 only (OpenH264) |
| Passwords in the config | DPAPI | key in Secret Service, or a private key file | key in Keychain, or a private key file |
| Start at login | Run key | XDG autostart | LaunchAgent |
| Package | Inno Setup installer (not yet tried on a clean PC) | `.deb`, `.rpm` | none |

Not built yet: the GPU decoding path and other hardening ([08.1](documentation/08.1-Plan-Phase-8.md)),
and macOS virtual cameras ([11](documentation/11-virtual-cameras-linux-macos-plan.md)). The
[documentation index](documentation/00-index.md) has the plans, progress records and the list
of checks that still need a person.

## Features

- **Camera brand presets:** the port and path for Hikvision, Dahua, Amcrest, Reolink, Tapo,
  UniFi Protect, Axis and Foscam.
- **ONVIF discovery:** **Find cameras** scans the network and reads each camera's streams.
- **Picture options** (**Picture...** in the stream dialog): rotate, flip, crop, show the name or
  the time, and choose what apps see when the stream drops (a "no signal" picture or the last
  frame).
- **On demand:** a stream can connect only while an app uses its camera.
- **Import / export** (**File > Export / Import streams**) to move streams between PCs
  (passwords are not included).
- **Tray:** `--minimized` starts in the tray, `--headless` runs without a window (Ctrl+C quits).

## Virtual cameras on Linux

The cameras are [v4l2loopback](https://github.com/v4l2loopback/v4l2loopback) devices: a kernel
module that makes `/dev/videoN` devices which RTSP Cam writes to and other apps read like a
webcam.

The `.deb` also installs `v4l2loopback-dkms`, which apt pulls in as a recommended package (on
Ubuntu it's in `universe`); on Fedora it comes from RPM Fusion as `akmod-v4l2loopback`. Both
packages add:

- `rtspcam-v4l2loopback.service`, which loads the module at boot with the right options for its
  version (the install runs it once too, so no reboot is needed unless Secure Boot is on).
- A udev rule (`70-rtspcam.rules`) that lets the logged-in user add and remove v4l2loopback
  devices, so each stream gets a camera named after it.

**What you get depends on the v4l2loopback version** (`modinfo -F version v4l2loopback`;
empty means 0.12):

| Version | Distros | Cameras |
|---|---|---|
| 0.13 and later | Debian 13 (0.15.0), Ubuntu 26.04 (0.15.3); rolling distros usually | One per stream, named after it, added and removed with the stream. On-demand streams connect only while an app uses the camera. |
| 0.12 | Ubuntu 22.04 and 24.04, Debian 12 (all 0.12.7) | Four devices made at boot, "RTSP Cam 1" to "RTSP Cam 4"; streams take them in turn, so names don't follow the streams, and at most four streams become cameras. On-demand streams run whenever their camera exists. |

**If the status line says the module is missing or not loaded:**

- *Kernel headers:* DKMS builds the module for the running kernel and needs its headers
  (`linux-headers-$(uname -r)`; desktop installs normally have them).
- *Secure Boot:* an unsigned DKMS module only loads once its key is enrolled. Ubuntu asks for a
  password while installing `v4l2loopback-dkms` and shows the enrolment screen (MOK) at the next
  boot; enter that password there. Until then `sudo modprobe v4l2loopback` fails with "Key was
  rejected by service".
- *Kernel updates:* DKMS rebuilds the module for each new kernel; if that fails there are no
  cameras until it is fixed (`sudo dkms status`).
- *Without the package* (built from source): install v4l2loopback yourself and load it with
  `sudo modprobe v4l2loopback exclusive_caps=1`. Without the udev rule only root may add
  devices, so with 0.13+ create them by hand
  (`sudo v4l2loopback-ctl add -x 1 -n "Front Door" /dev/video10`, named like the stream; RTSP
  Cam uses a free device with its stream's name), or run
  `sudo installer/linux/rtspcam-v4l2loopback` from this repository after `sudo modprobe -r
  v4l2loopback`.

Apps only list a camera while RTSP Cam is running (the devices are "exclusive caps": they show
up as cameras only while something writes to them). Browsers and Discord may need a restart to
see a camera added while they were open.

## Building from source

### Requirements

All OSes:

- [Rust](https://rustup.rs/) 1.92 or later. The toolchain is pinned in `rust-toolchain.toml`,
  and rustup installs it automatically.
- A C and C++ compiler (OpenH264 is built from source).

Windows:

- Windows 11 (build 22000 or later) for the virtual cameras.
- **Visual Studio Build Tools** with the *Desktop development with C++* workload (MSVC
  linker and Windows SDK):

  ```powershell
  winget install Microsoft.VisualStudio.2022.BuildTools --override "--quiet --wait --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended"
  ```

Linux (checked on Debian bookworm):

- Build: `build-essential`, `pkg-config` and `libfontconfig1-dev` (Slint's font lookup links
  fontconfig); no other GUI `-dev` packages are needed.
- Run: the X11 or Wayland client libraries, loaded at run time. On a minimal X11 system:
  `libx11-6 libx11-xcb1 libxcursor1 libxrandr2 libxi6 libxkbcommon-x11-0`, plus fonts
  (`fonts-dejavu-core`) and OpenGL (`libgl1 libegl1`; without it Slint draws in software).
- Optional: a Secret Service (GNOME Keyring, KWallet) for passwords; without one the key is
  kept in a file only you can read. A StatusNotifierItem host for the tray (KDE; GNOME needs
  the AppIndicator extension). An XDG desktop portal for the Import/Export file dialogs.
- v4l2loopback for the cameras ([above](#virtual-cameras-on-linux)).

macOS:

- Xcode Command Line Tools.

Optional everywhere:

- [mise](https://mise.jdx.dev/) for the task shortcuts below.
- `mediamtx` + `ffmpeg`, or Docker, for the [test RTSP server](tools/test-rtsp/README.md).

### Run the app

```sh
cargo run -p rtspcam-app                 # debug build, opens the window
```

On Windows, run the commands from the repository root in PowerShell.

> If you build from Git Bash, its `/usr/bin/link` can shadow MSVC's `link.exe`. Build
> from PowerShell or a *Developer PowerShell for VS* prompt instead.

**Windows: register the camera DLL** (one-time, needs admin). Without this the preview works
but each camera shows "Access is denied":

```powershell
cargo build -p rtspcam-cli -p rtspcam-vcam
./tools/vcam/install-dev.ps1             # asks for elevation (UAC)
```

Rebuild and re-run the script after changing the DLL. `./tools/vcam/install-dev.ps1 -Uninstall`
removes it.

### Release build and packages

```sh
cargo build -p rtspcam-app --release     # -> target/release/rtspcam (rtspcam.exe on Windows)
```

On Windows the release build is a windowed app (no console) with the manifest embedded, and
the exe is self-contained apart from `rtspcam_vcam.dll`, which is built with
`cargo build -p rtspcam-vcam --release` and registered with
`./tools/vcam/install-dev.ps1 -Configuration release`.

The Windows installer needs [Inno Setup 6](https://jrsoftware.org/isinfo.php)
(`winget install JRSoftware.InnoSetup`):

```powershell
./tools/installer/build.ps1             # -> installer\out\RtspCam-<version>-setup.exe
```

It installs to `C:\Program Files\RtspCam`, registers the virtual camera DLL, and removes both on
uninstall (your config is kept). Pushing a `v*` tag builds the installer, `.deb` and `.rpm` in
CI and attaches them to a GitHub release; see [08](documentation/08-progress-phase-7.md) for
signing and [13](documentation/13-progress-phase-l.md) for the Linux packages.

### Build and test

```sh
cargo build --workspace
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all
```

The same commands work on Linux and macOS; the Windows-only crates (`rtspcam-vcam`,
`rtspcam-vcam-mgr`) build as empty crates there. CI runs them on all three OSes.

Or with mise: `mise run test`, `mise run lint`, `mise run ci`.

### Layout

```
crates/
  rtspcam-core/       config model (JSON), Protocol + URL builder, encrypted secrets, validation,
                      atomic save, migrations, file watching, logging, shared constants
  rtspcam-ipc/        app ↔ virtual camera frame protocol, over any byte stream; placeholder pictures
  rtspcam-pipeline/   RTSP ingest (retina), OpenH264 decoding, scaling, frame bus, reconnects
  rtspcam-onvif/      ONVIF: WS-Discovery scan, GetProfiles / GetStreamUri
  rtspcam-platform/   everything OS-specific, behind traits: virtual cameras, secrets,
                      autostart, single instance, frame transport, MF decoder
                      (windows/, linux/, macos/, unix/)
  rtspcam-engine/     camera manager, preview, status, stream form, overlay (portable)
  rtspcam-app/        rtspcam: the Slint desktop app (ui/*.slint + src/ui/)
  rtspcam-cli/        rtspcam-cli: developer tool (`vcam` subcommand Windows-only)
  rtspcam-vcam/       rtspcam_vcam.dll: the Media Foundation custom media source (Windows)
  rtspcam-vcam-mgr/   safe wrapper over MFCreateVirtualCamera (Windows)
tools/test-rtsp/      mediamtx + ffmpeg test streams
tools/vcam/           developer install of the media source DLL
tools/installer/      builds the Windows installer
installer/            Inno Setup script; linux/ has the v4l2loopback service, udev rule, .deb/.rpm files
documentation/        plans and progress records (start at 00-index.md)
```

## Configuration

Everything lives in one JSON file, `config.json`, in the per-user config folder:

| | Config | Logs |
|---|---|---|
| Windows | `%APPDATA%\RtspCam\` | `%LOCALAPPDATA%\RtspCam\logs\` |
| Linux | `~/.config/RtspCam/` (`$XDG_CONFIG_HOME`) | `~/.local/share/RtspCam/logs/` |
| macOS | `~/Library/Application Support/RtspCam/` | `~/Library/Application Support/RtspCam/logs/` |

`rtspcam-cli config path` prints it. The format is the same on every OS:

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
      "picture": { "rotate": "90", "show_name": true, "on_disconnect": "freeze_last_frame" },
      "on_demand": true
    }
  ]
}
```

- Every field is optional and has a default. Unknown fields are kept when the file is saved.
  `start_with_windows` is the start-at-login setting on every OS.
- Passwords are encrypted. Windows: DPAPI (current user only), stored as `dpapi:...`. Linux and
  macOS: with a key kept in Secret Service or the Keychain (`keyring:...`), or, if neither is
  available, in a `secret.key` file next to the config that only you can read (`keyfile:...`).
  A password that can't be decrypted (for example a config copied from another PC or OS) is
  kept, and the stream asks you to enter it again. A plain-text password typed into the file
  by hand is encrypted the next time the app saves.
- Saves are atomic, and the previous version is kept as `config.json.bak`.
- Edits made to the file while the app runs are picked up automatically.
- `picture` is optional (all of its fields are too): `rotate` is `"0"`, `"90"`, `"180"` or `"270"`
  clockwise; `crop` is the percentage to cut from each side (`left`, `top`, `right`, `bottom`,
  at most 80 each); `show_name` and `show_time` draw text in the bottom-left corner;
  `on_disconnect` is `"no_signal"` (default) or `"freeze_last_frame"`. Order: crop, rotate, flip,
  text, then scaling to the output size.

## Logs

Logs are written to the logs folder in the table above (rotated daily, 7 files kept);
**Help > Open logs folder** opens it. Set `RTSPCAM_LOG` to override the level, for example
`RTSPCAM_LOG=debug`. On Windows the camera DLL logs to `%ProgramData%\RtspCam\logs\vcam.log`.

## Developer CLI

```sh
cargo run -p rtspcam-cli -- config path
cargo run -p rtspcam-cli -- config show       # passwords redacted
cargo run -p rtspcam-cli -- config validate
cargo run -p rtspcam-cli -- config add-test-streams
cargo run -p rtspcam-cli -- config export streams.json   # no passwords
cargo run -p rtspcam-cli -- config import streams.json   # new ids, names made unique

./tools/test-rtsp/start.ps1 -Docker -Detach                       # test RTSP server
cargo run -p rtspcam-cli -- probe rtsp://127.0.0.1:8554/h264-720p
cargo run -p rtspcam-cli -- view --all --seconds 30                # every stream in the config

cargo run -p rtspcam-cli -- onvif discover                         # ONVIF cameras on the LAN
cargo run -p rtspcam-cli -- onvif streams 192.168.1.50 -u admin -p secret
cargo run -p rtspcam-cli -- onvif streams 192.168.1.50:2020 --credentials-from "Front Door"

# Windows only (after registering the DLL):
cargo run -p rtspcam-cli -- vcam add --name "Test" --pattern       # a camera until Ctrl+C
cargo run -p rtspcam-cli -- vcam add --name "Front" --stream rtsp://127.0.0.1:8554/h264-720p
```
