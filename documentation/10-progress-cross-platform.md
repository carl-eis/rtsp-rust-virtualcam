# 10 — Progress: RTSP Cam on Windows, Linux and macOS

Status (2026-10-09): steps A–F of the [plan](09-cross-platform-plan.md) are done on
`feat/cross-platform`, and CI passes on Windows, Linux and macOS
([PR #1](https://github.com/carl-eis/rtsp-rust-virtualcam/pull/1)). What is left needs a
person: the checks in §4.

| Step | Commit | State |
|---|---|---|
| Plan | `054a707` | [09](09-cross-platform-plan.md) |
| A. Extract `rtspcam-platform` (Windows code moved, no behaviour change) | `a4af991` | done |
| B. Extract `rtspcam-engine`; portable overlay text | `433766a` | done |
| C. Linux and macOS platform services | `3639734` | done |
| D. Slint UI, winsafe removed | `dd7e0ae` | done |
| E. CI on Windows, Linux, macOS | `9cf49d1`, `a4d6362` | done; see §7 |
| F. Docs | this commit | done |

## 1. What was done

### Crate layout

```
rtspcam-core        portable: config, paths, logging, Secret + SecretStore hook, FileReplace
rtspcam-ipc         portable: protocol, FrameSource, generic client, serve() over a FrameListener
rtspcam-pipeline    portable: RTSP, OpenH264, scaling; PlatformDecoders hook
rtspcam-onvif       portable (unchanged)
rtspcam-platform    every OS-specific piece, behind traits: windows/, linux/, macos/, unix/
rtspcam-engine      portable: CameraManager, Preview, status, form, probe, discovery, overlay
rtspcam-app         Slint UI + main() (binary only)
rtspcam-cli         portable; `vcam` subcommand Windows-only
rtspcam-vcam        Windows DLL (only its pipe-opening line and two constants changed)
rtspcam-vcam-mgr    Windows-only, unchanged
```

`cfg(target_os/windows/unix)` appears only in `rtspcam-platform`, the two Windows-only crates
and the CLI's `vcam` gating. `tools/ci/check-os-cfg.sh` enforces this in CI (`mise run os-cfg`
locally).

### Traits (all re-exported from `rtspcam_platform`)

| Trait | Windows | Linux | macOS |
|---|---|---|---|
| `VirtualCameraBackend` | MF virtual cameras; the backend now also serves the camera's pipe | `UnsupportedCameras` | `UnsupportedCameras` |
| `SecretStore` (in core) | DPAPI, `dpapi:` (unchanged) | key in Secret Service, `keyring:`; fallback key file, `keyfile:` | key in Keychain, `keyring:`; same fallback |
| `FileReplace` (in core) | `ReplaceFileW`, fallback copy+rename (unchanged) | copy + rename | copy + rename |
| `Autostart` | HKCU `Run` value `RtspCam` (unchanged) | `~/.config/autostart/rtspcam.desktop` | `~/Library/LaunchAgents/io.github.carl-eis.rtspcam.plist` |
| `SingleInstance` | `Local\RtspCam.SingleInstance` + `Local\RtspCam.Show` (unchanged) | lock file + Unix socket | lock file + Unix socket |
| `FrameTransport` | `\\.\pipe\rtspcam\<id>` with the same DACL (unchanged) | Unix sockets in a 0700 runtime folder | same |
| `PlatformDecoders` (in pipeline) | Media Foundation (unchanged) | none (OpenH264) | none (OpenH264) |

A v4l2loopback backend means one type in `platform/src/linux/` implementing
`VirtualCameraBackend` and returning it from `linux::camera_backend()`. The trait hands the
backend the camera's `FrameSource`, which works for push backends (call `next_frame` at the
device rate) and serving backends (pass it to `rtspcam_ipc::server::serve`).

### UI (Slint 1.18)

- `crates/rtspcam-app/ui/*.slint`, fluent style on every OS; Rust controllers in `src/ui/`.
- Dialogs are modal sheets inside the main window (Slint has no owned/modal windows).
- Tray: Slint's built-in `SystemTrayIcon` (not `tray-icon`/`muda`; no GTK).
- Preview: worker thread scales + converts to RGBA, hands a `SharedPixelBuffer` to the UI
  thread, at most one in flight.
- File dialogs: `rfd`. Message boxes: sheets; fatal start-up error: `rfd::MessageDialog`.
- App icon: `crates/rtspcam-app/assets/icon.png`, compiled in.

## 2. Decisions (beyond the plan)

| Topic | Decision |
|---|---|
| Branch | Used the existing `feat/cross-platform` (identical to `master` when work started). |
| MSRV | `rust-version` raised 1.85 → 1.92 (Slint needs it). Fixed the MSRV-gated clippy lints this enabled (`is_multiple_of`, `as_chunks`). |
| Overlay font | Inter (OFL 1.1, `crates/rtspcam-engine/assets/`), semibold via its weight axis. Text looks slightly different from the old Segoe UI on Windows. |
| Pipe failure | If a camera's pipe can't be created (another copy serving it), the Windows backend now fails `create` without creating the MF camera. Before, the camera was created anyway and the status showed the pipe error. Same message. |
| Window icon | A PNG via `@image-url`: Slint 1.18 never applies a window icon built from a pixel buffer (its icon cache key is `None`). |
| Small text changes | About says "virtual webcams" (no "Windows"); the Find cameras hint says "the firewall"; the autostart error names the setting's label. The "still running in the tray" balloon is gone (Slint's tray has no balloons). Tray opens on single left-click. |

## 3. Verified

On Windows 11 (this machine):

- `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test
  --workspace` pass after every step, also with `RTSPCAM_TEST_SERVER` (mediamtx in Docker),
  including the moved Media Foundation pipeline test.
- Ran the Slint app against the real config: live preview of a Tapo camera through Media
  Foundation; status "Camera "LivingRoom" is available to apps" (camera created via the DLL);
  Add stream (URL paste, validation), Picture, Find cameras (found both ONVIF cameras),
  Settings, Help menu; a second launch exits and leaves the first running; closing the window
  ends the process in ~160 ms. Nothing was saved to the real config.

On Linux (Debian bookworm container, `rust:1-bookworm`):

- fmt, clippy `-D warnings`, all tests for the whole workspace (including the app).
- The app under Xvfb, driven with xdotool: added a password-protected stream through the UI,
  Test connection (OpenH264), live preview at 30 fps, status "Virtual cameras are not
  supported on this platform yet", config saved with a `keyfile:` password and a 0600 key
  file (no D-Bus in the container, so the keyring fallback was used), File > Quit exits.
- The CLI with the real secret store: `dpapi:` values from Windows lock and ask for the
  password again instead of failing.

On macOS (GitHub `macos-latest`, Apple silicon): fmt, clippy `-D warnings`, all tests and the
release build of the whole workspace, including the app and OpenH264. The app has not been
started on a Mac.

In CI on `windows-latest` and `ubuntu-latest` too: the same checks, plus the installer build
on Windows.

## 4. Not verified

To verify on Windows (needs a person):

1. Cameras still appear and stream in Discord, the Camera app, OBS, Chrome.
2. DPAPI passwords saved by the old build still decrypt (tests cover new values only).
3. Start with Windows: the Run value is written and the app starts in the tray at sign-in.
4. Single instance against a copy of the old (winsafe) build.
5. Tray: icon, menu (Open / Pause all / Quit), left-click opens, re-added after an Explorer
   restart; minimize to tray hides the taskbar button.
6. Install/uninstall with the Slint exe (the installer builds in CI, but has not been run);
   the release build with `windows_subsystem` and the manifest; GPU-less VM (software
   renderer fallback).

Elsewhere:

7. macOS at run time (it builds and its tests pass in CI): the window, Keychain prompts for an
   unsigned binary, LaunchAgent at login, menu bar tray, file dialogs.
8. Linux on a real desktop: Secret Service keyring path, tray on KDE/GNOME+AppIndicator, XDG
   portal file dialogs, autostart at login, Wayland (minimize-to-tray can't detect minimize
   there; the window just minimizes).

## 5. Known gaps

- H.265 and MJPEG decode only on Windows (platform decoders); elsewhere only H.264.
- On-demand streams on push camera backends (added in phase G, [12 §3](12-progress-phase-g.md#3-on-demand-differs-from-windows-when-the-os-cant-tell)):
  if the OS can't say whether an app is reading the camera, the stream runs for as long as its
  camera exists, not only while an app uses it as on Windows.
- Linux runtime needs X11/Wayland client libraries (`libx11-6 libxcursor1 libxrandr2 libxi6
  libxkbcommon-x11-0` on X11), loaded at run time.
- Linux builds need `libfontconfig1-dev`: Slint's font lookup (`fontique`) links fontconfig.
  The `rust:1-bookworm` image used during C and D has it, so this only showed up on the
  GitHub runner. `RUST_FONTCONFIG_DLOPEN` does not help: it switches `yeslogic-fontconfig-sys`
  to dlopen, but `fontique` only uses that through its own `fontconfig-dlopen` feature, which
  Slint does not expose.

## 6. How to resume

1. Review and merge PR #1.
2. Work through §4.
3. Virtual cameras on Linux and macOS: [11](11-virtual-cameras-linux-macos-plan.md).

## 7. Step E (CI)

- `.github/workflows/ci.yml`: a matrix over `windows-latest`, `ubuntu-latest` and
  `macos-latest` (no fail-fast) running fmt, clippy `-D warnings`, tests and a release build,
  and uploading the binaries per OS. Windows also runs the DLL-set clippy (`-p rtspcam-core
  --no-default-features`) and builds the Inno Setup installer from the same release binaries
  (`build.ps1 -SkipBuild`), uploaded as `RtspCam-setup-<sha>`. CI now also runs on pushes to
  `feat/**` branches.
- Job `os-cfg`: `tools/ci/check-os-cfg.sh` fails if `cfg(windows|unix|target_os|...)` appears
  in a `.rs` file or `Cargo.toml` outside `rtspcam-platform`, `rtspcam-vcam`,
  `rtspcam-vcam-mgr`, the CLI's `main.rs`/`Cargo.toml` (`vcam` gating) and the app's
  `windows_subsystem` line. Checked both ways: passes on the tree, fails on an added
  `#[cfg(target_os = "linux")]` in core.
- `rust-toolchain.toml` no longer pins `x86_64-pc-windows-msvc`; each OS uses its host target.
  The static-CRT flag in `.cargo/config.toml` is per target and still applies on Windows.
- `release.yml` is unchanged (Windows installer on `v*` tags), apart from the toolchain step
  below.

Fixes after the first run on GitHub:

- macOS failed before any build: the runner's preinstalled, older stable was used, because
  `rustup show active-toolchain` succeeds without updating. That cargo rejects
  `default-features = false` on a workspace dependency whose workspace entry has the defaults
  on (`rtspcam-platform` → `rtspcam-pipeline`); newer cargo accepts it silently and enables
  them anyway. Now the workspace entry for `rtspcam-pipeline` has `default-features = false`
  (like `rtspcam-core`), and the app, CLI, engine and platform's dev-dependency turn them on.
  `rtspcam-platform` built alone now really leaves out OpenH264. The toolchain step in both
  workflows runs `rustup update stable` first.
- Linux failed on `yeslogic-fontconfig-sys` (no `libfontconfig1-dev` on the runner); the job
  installs it now (see §5).
- Checked locally: on Windows fmt, both clippy runs and all tests; on Linux (Docker, below)
  the cfg check, fmt, clippy and all tests.

Linux checks used during this work (Docker):

```sh
docker run --rm -v "$PWD:/src:ro" -v rtspcam-target:/target rust:1-bookworm \
  sh -c 'cd /src && sh tools/ci/check-os-cfg.sh && CARGO_TARGET_DIR=/target cargo clippy --workspace --all-targets -- -D warnings && CARGO_TARGET_DIR=/target cargo test --workspace'
```
