# 13 — Progress: phase L (Linux virtual cameras through v4l2loopback)

Status (2026-10-10): phase L of [11](11-virtual-cameras-linux-macos-plan.md#4-phase-l--linux-v4l2loopback)
is built on `feat/linux-virtual-camera`, following phase G ([12](12-progress-phase-g.md)). On
Linux each stream becomes a v4l2loopback camera. It was checked against the real module on a
real kernel (§4) with ffmpeg and v4l2-ctl as the camera apps; browsers, call apps and a real
desktop session still need a person (§5).

| Step (11 §4) | State |
|---|---|
| L1. Spike | done, on the WSL2 kernel (6.18) with v4l2loopback 0.15.3 built from source; 0.12.7 partly (§4) |
| L2. Backend `V4l2Loopback` | done |
| L3. `.deb` and `.rpm` | done; install-tested in containers (§4) |
| L4. Verify and document | README and this record done; app checks need a person (§5) |

## 1. L1 findings

| Question (11 §4 L1) | Answer |
|---|---|
| Versions shipped | Ubuntu 22.04 and 24.04, Debian 12: **0.12.7**. Debian 13: 0.15.0. Ubuntu 26.04: 0.15.3. Fedora: not in the main repositories (RPM Fusion only). |
| Control device | `/dev/v4l2loopback` since **0.13.0**. 0.12 has none: devices are made only by module options at load. |
| Control ioctls | 0.13 and 0.14 use `0x4C80`/`0x4C81`/`0x4C82` (add, remove, query); 0.15 added `_IOW('~', …)` numbers and still accepts the old ones, so the backend uses the old ones. `REMOVE` takes the device number itself, not a pointer, and fails with `EBUSY` while anything has the device open. `struct v4l2_loopback_config` (72 bytes) is the same from 0.13 to 0.15. |
| Version tag | 0.13+ set `MODULE_VERSION` (`modinfo -F version`, `/sys/module/v4l2loopback/version`); **0.12 has none** (empty). |
| Pixel format | YUYV chosen without a browser test: it is the format every UVC webcam offers, so every Linux camera stack reads it. ffmpeg and v4l2-ctl read it at the set size and rate. Chrome, Firefox, Zoom etc. still need checking (§5). |
| `exclusive_caps` | Confirmed: a device with exclusive caps reports *Video Output* only until something **writes a frame**; setting the format is not enough. After that it reports *Video Capture*, which is what apps list. When the writer closes it goes back to output-only, so a device left behind is invisible to apps. |
| Reader events | 0.13+ send `V4L2_EVENT_PRI_CLIENT_USAGE` (`V4L2_EVENT_PRIVATE_START + 0x08E00000 + 1`, payload `u32` count) on capture `STREAMON`/`STREAMOFF`, and once on subscribe with the current state. Seen working: every reader start and stop toggled the RTSP connection of an on-demand stream. 0.12 rejects the subscription. Readers using `read()` instead of mmap streaming aren't counted. |
| Access | The control device is `crw------- root` by default, so a udev rule is needed (L3). `/dev/videoN` access for the logged-in user comes from systemd's own `uaccess` rule for `video4linux`. Neither could be checked without a logind session (§5). |
| Secure Boot | Not testable here (§5). |
| Device names | `card_label` (31 bytes plus NUL) is the name apps show and `/sys/class/video4linux/videoN/name`. Module-option labels with spaces need quotes for the kernel's parser: `card_label="\"RTSP Cam 1,RTSP Cam 2\""`. |

## 2. What was done

### Backend (`crates/rtspcam-platform/src/linux/`)

- `v4l2.rs`: the few ioctls on top of `libc` (no new C dependency): `QUERYCAP`, `S_FMT`,
  `S_PARM`, `SUBSCRIBE_EVENT`, `DQEVENT`, and the control device's add and remove. Struct sizes
  and ioctl numbers are pinned by tests on 64-bit targets.
- `camera.rs`: `V4l2Loopback`, returned by `linux::camera_backend()`.
  - `check()`: `Ok` when the module is loaded. Otherwise `Unsupported` with what to do
    ("install v4l2loopback-dkms (Debian, Ubuntu) or akmod-v4l2loopback (Fedora, from RPM
    Fusion)…", or "installed but not loaded. Restart the computer…", told apart through
    `modules.dep`). Not `Failed`: `main()` refuses to start on `Failed` (that is for Windows 10).
  - `create(spec)`: finds the device, sets it to YUYV at `spec.preferred` (`S_FMT`, checked, and
    `S_PARM` for the rate), writes one placeholder frame (exclusive caps, §1), subscribes to the
    reader events and starts a `Pusher` whose sink writes each frame. Which device:
    1. a free v4l2loopback device already labelled with the stream's name (one left by a crash,
       or still open in an app when its camera was removed);
    2. else a new one through the control device, labelled with the name (waits up to 3 s for
       udev to make the node and set its permissions);
    3. else (0.12, or no access to the control device) a free boot-time device labelled
       "RTSP Cam N".
    "Free" means `S_FMT` on the output doesn't fail with `EBUSY`.
  - `remove(id)` / `remove_all()`: stop the pusher (closing the device), then remove the device
    if the control device is usable. A device still open in an app (`EBUSY`) is retried on
    each later create or remove, and otherwise stays until reboot (invisible to apps).
  - The sink's `has_reader` reads the queued events without blocking (`poll` for `POLLPRI`,
    zero timeout) and returns `Some(count > 0)`; `None` on 0.12.

### Packaging (`installer/linux/`, `crates/rtspcam-app/Cargo.toml`, `release.yml`)

| File | Installed as | Does |
|---|---|---|
| `rtspcam-v4l2loopback` | `/usr/lib/rtspcam/` | Loads the module unless it is loaded: 0.13+ with `exclusive_caps=1` (one spare device for OBS and the like), 0.12 with 4 devices "RTSP Cam 1..4", exclusive caps. Never fails. |
| `rtspcam-v4l2loopback.service` | `/usr/lib/systemd/system/` | Runs the script at boot (enabled by the install script). |
| `70-rtspcam.rules` | `/usr/lib/udev/rules.d/` | `TAG+="uaccess"` on `/dev/v4l2loopback`, so the logged-in user can add and remove devices. |
| `rtspcam.desktop`, `assets/icon.png` | `/usr/share/applications/`, hicolor 256×256 | Menu entry. |
| `deb/postinst`, `prerm`, `postrm`; `rpm/post`, `preun`, `postun` | maintainer scripts | Reload udev rules and trigger the control device, enable the service, run the script once, say what's missing. |

- `.deb` with `cargo-deb`: `Depends: $auto` (libc6 ≥ 2.35, libfontconfig1, libstdc++6),
  `Recommends: v4l2loopback-dkms`.
- `.rpm` with `cargo-generate-rpm` (0.21): `Recommends: v4l2loopback`. Requirements are listed
  by hand (`auto-req = "no"`): its built-in finder writes `libc.so.6(GLIBC_2.27)[WEAK]`, which no
  package provides, so the first build couldn't be installed on Fedora.
- `release.yml`: new job `linux-packages` on `ubuntu-22.04` (glibc 2.35, so the binary also runs
  on Ubuntu 22.04 and Debian 12), strips the binaries (121 MB → 28 MB for the app), builds both
  packages, uploads them and attaches them to the release.
- `.gitattributes`: `installer/linux/**` is checked out with LF, so the scripts work when
  packaged from a Windows checkout.

## 3. Decisions (beyond the plan)

| Topic | Decision |
|---|---|
| Module options at boot | A boot service with a script, not `/etc/modules-load.d` + `/etc/modprobe.d` as 11 §4 L3 planned: the right options depend on the module's version, which a distro upgrade can change (0.12 → 0.15). Static `modprobe.d` options for 0.12's fixed devices would add four dead cameras on 0.13+. |
| Module already loaded | The script leaves it alone (loaded by hand or by OBS). With 0.13+ the app still adds its own devices; with 0.12 it only finds "RTSP Cam N" devices if they exist. |
| Missing module | `check()` is `Unsupported` (with the explanation), not `Failed`, and `camera_backend()` always returns `V4l2Loopback` (11 §4 L2 had it return `UnsupportedCameras`): the status says what to install instead of "not supported yet". |
| Pixel format | YUYV only, for every camera (§1). The pusher can write I420 if a device needs it. |
| Reuse by name | A free device with the stream's label is used before adding one, so crashes and devices still open in apps don't pile up duplicates. |
| Spare device on 0.13+ | Kept (the module's default single device, with exclusive caps) so OBS's own virtual camera still finds a device. |
| Help page | Not added: the plan's "Help > Virtual cameras on Linux" would change the UI, which 11 §1 rules out; the status text points to the README section instead. |

## 4. Verified

**Real module on a real kernel.** WSL2's kernel (6.18.40.1) has V4L2 as modules; the kernel
was built from Microsoft's tag with `/proc/config.gz` (for `Module.symvers`) and v4l2loopback
0.15.3 against it, then loaded into the running kernel from a privileged container with the
VM's `/dev`. The release build of the app ran `--headless` against mediamtx with an ffmpeg
test pattern (H.264 720p30) and an unreachable second stream:

- Two devices appeared, labelled "Front Door" and "Garage (offline)", next to the spare one;
  each reported *Video Capture*, `YUYV` at the stream's size, and its frame rate (30 and 15
  fps) once the app had written to it.
- `v4l2-ctl --stream-mmap --stream-count=90` and `ffmpeg -f v4l2` read the stream at 30 fps; a
  frame converted to PNG shows the test pattern correctly. The offline stream showed the
  "Connecting..." placeholder.
- On-demand: each reader start logged "an app started using the camera" and connected the RTSP
  stream; each stop disconnected it.
- Ctrl+C (SIGINT): both devices were removed. `kill -9`: the devices stayed, as output-only
  (invisible to apps); the next start used them again by name and removed them on exit.
- A device added with `S_FMT` but no write stayed output-only; after one write it was a
  capture device (the reason for the first placeholder frame).

**0.12.7** (what Ubuntu 24.04 ships) doesn't build on 6.18 as is; patched for the kernel API
changes (`strscpy`, `timer_delete_sync`, `timer_container_of`, `v4l2_fh_add/del` taking the
file). The boot script loaded it with the four "RTSP Cam N" devices (labels with spaces came
out right, a second run did nothing), and the app claimed two of them, logging
`on_demand_supported=false`. Reading from it then crashed the WSL VM: most likely the
forward-port, since the stock module needs none of these patches on Ubuntu 24.04's 6.8 kernel.
Not retried. Reading on 0.12 is therefore unverified (§5).

**Packages.** The `.deb` installed, ran and removed cleanly on Ubuntu 22.04, Ubuntu 24.04 and
Debian 12, and the `.rpm` on Fedora 42 (containers, no systemd or udev, so those branches of
the scripts were skipped): install messages without the module, the app's status message,
`rtspcam-cli --version`, nothing left in `/usr/lib/rtspcam` after removal. shellcheck passes on
every script.

**Checks.** `cargo fmt --check`, `tools/ci/check-os-cfg.sh`, and in `rust:1-bookworm`
`cargo clippy --workspace --all-targets -D warnings` and `cargo test --workspace`. New tests:
struct sizes and ioctl numbers, label truncation at a character boundary, and that the
backend's fixed-device count and label prefix match the boot script.

## 5. Not verified (needs a person, on a real Linux desktop)

- Each stream as a camera in Chrome, Firefox, Zoom, Teams in a browser, Discord, OBS and
  `cheese`/`guvcview`, on Ubuntu 24.04 (GNOME, Wayland; module 0.12.7) and a distro with 0.13+
  (Debian 13 or Ubuntu 26.04), plus one KDE desktop. In particular: does Chrome list the
  device, and does every app take YUYV?
- 0.12.7 on its own kernel: reading frames, two streams on two "RTSP Cam" devices, and what
  happens when a fixed device gets a different size on the next start (0.12 may keep the
  old format while a reader holds it).
- The udev rule: the logged-in user can open `/dev/v4l2loopback` (`getfacl`), cameras are added
  without root, and the boot service runs before login.
- Secure Boot: `apt install` of the `.deb` with `v4l2loopback-dkms`, MOK enrolment, cameras
  after the reboot.
- Logout or shutdown: the GUI app now quits on SIGTERM like the tray's Quit (§8), removing its
  devices. Checked headless only; the GUI path at a real logout still needs a person.
- An edit while a call app reads the camera (§8): the app keeps getting pictures on 0.13+ and
  0.12, including after a size change (0.12 may refuse a new size while a reader holds it).
- Two users logged in at once, each running the app (devices are per name and "free" is per
  writer, so it should work).

## 6. Known gaps

- On 0.12 the cameras are "RTSP Cam 1–4", not the stream names, at most four of them, and
  on-demand streams run all the time.
- A reader's first frame can be the placeholder written when the camera was made, with an old
  timestamp (ffmpeg's `-t` then counts from it). Real apps just show one placeholder frame.
- Readers that use `read()` instead of mmap streaming don't count as readers on 0.13+.
- `$auto` doesn't see the X11/Wayland/GL libraries that winit loads at run time; desktop
  installs have them (README Requirements).
- The `.rpm` license field says `Proprietary` because the project has no license yet.

## 7. How to resume

1. Do §5 on a real machine. Install the `.deb` from the `linux-packages` job (run
   `release.yml` with workflow_dispatch to get one without a tag).
2. Phase M (macOS), [11 §6](11-virtual-cameras-linux-macos-plan.md#6-phase-m--macos-coremediaio-camera-extension).

## 8. Fixes after phase L (2026-10-10, `feat/linux-sigterm-and-device-removal`)

| Problem | Fix |
|---|---|
| SIGTERM (logout, shutdown, `kill`, `systemctl stop`) ended the app without removing its devices. | `rtspcam_platform::desktop::termination_requested()` resolves on SIGTERM or SIGHUP on Linux and macOS (never on Windows, where the cameras go with the process). Headless waits for it next to Ctrl+C; the GUI quits through the event loop like the tray's Quit. Checked in a container: SIGTERM logs "received SIGTERM", shuts down in order and exits 0. |
| Any edit re-created the camera: the manager removed it, then added it. Removing a device an app still has open fails (`EBUSY`), so it lingered; on 0.12 the stream then took the first free "RTSP Cam N", possibly another one, and the app in the call read a device nobody wrote to. | The manager no longer calls `remove` for an edited stream; it calls `create` again for the same id (re-creating replaces, on every backend). On Linux, `create` for an existing id stops the old pusher and takes the **same** device again (`keep_device`). A renamed stream gets a new device with the new label if the old one can be removed; if an app has it open, the camera stays on it under the old label. Only if the device can't be taken again (another writer, a format it refuses) does it fall back to finding a device as for a new camera. |

The manager test `edits_rebuild_only_what_changed_and_removals_remove` now checks that an edit
doesn't call `remove`, and that removing the stream does.

