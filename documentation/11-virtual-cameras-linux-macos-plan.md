# 11 — Plan: virtual cameras on Linux and macOS

Status: plan (2026-10-09), nothing built yet. It follows the cross-platform work in
[09](09-cross-platform-plan.md) and [10](10-progress-cross-platform.md), which left both OSes
with `UnsupportedCameras`: preview, config and discovery work, but no camera appears in other
apps.

Each phase below is a separate piece of work that can ship on its own: phase G (shared groundwork)
first, then L (Linux) and M (macOS) in either order. Facts marked *check* are from memory
or documentation and must be confirmed on a real system in that phase's first step.

## 1. Goal and limits

1. On Linux and macOS, each enabled stream shows up as its own camera in other apps (browsers,
   Zoom, Teams, Discord, OBS), named after the stream, like on Windows.
2. The camera manager, engine, UI and config do not change. Each OS gets one new
   `VirtualCameraBackend` in `rtspcam-platform`, as 09 §4.1 intended.
3. Windows behaviour does not change.

Out of scope: audio, Flatpak/Snap packaging (neither can load a kernel module or install a
system extension), and the Mac App Store.

## 2. How other software does it

| | Linux | macOS |
|---|---|---|
| Mechanism | v4l2loopback: a kernel module that creates `/dev/videoN` devices; whatever one process writes, apps read as a webcam. | A CoreMediaIO Camera Extension (macOS 12.3+): a system extension bundled inside the `.app` and approved by the user once in System Settings. The old DAL plugins are deprecated and newer macOS no longer loads them. |
| OBS | Doesn't ship the module. On **Start Virtual Camera** it runs `pkexec modprobe v4l2loopback ...` and writes frames to the device. | Ships a camera extension in `OBS.app` (OBS 30+, macOS 13+), signed and notarized with OBS's Developer ID. |
| What it needs from us | The distro's `v4l2loopback-dkms` package and a little udev setup (§5). | An Xcode target in Swift, an `.app` bundle, a paid Apple Developer account, the system-extension entitlement and notarization. |

Both are **push** models from the app's side: the app produces frames and hands them to a
device. Windows is the opposite (the DLL in Frame Server asks for each frame over a pipe).

## 3. Phase G — shared groundwork (portable, small)

Both backends need a loop that takes frames from a `FrameSource` at a steady rate, draws the
"No signal"/"Connecting..." picture when there are none, and stops on `remove`. Today that
logic lives only in the Windows DLL (`rtspcam-vcam/src/stream.rs` `deliver()` and
`placeholder.rs`).

1. Move `placeholder.rs` (5×7 bitmap font, no OS calls) from `rtspcam-vcam` to `rtspcam-ipc`,
   which the DLL already depends on. The DLL imports it from there; its output doesn't change.
2. Add `rtspcam_platform::push::Pusher`: a thread per camera that, at the camera's fps, calls
   `next_frame`, uses the last frame again until it is `STALE_AFTER` old, then the placeholder from
   `status()`, and passes each picture to a `FrameSink` (`fn write(&mut self, frame: &[u8])`).
   Portable code, so no OS `cfg` (it lives in platform because only backends use it).
3. On-demand streams: the pusher calls `client_connected` only while the sink reports a
   reader (`FrameSink::has_reader() -> Option<bool>`). If the OS can't tell (`None`), it
   counts as one reader for as long as the camera exists, so the stream runs while its camera
   is on. That differs from Windows, and 10 must record it.
4. Pixel formats: add `Yuyv` and `I420` to `rtspcam_ipc::PixelFormat` (new codes; the DLL
   never asks for them) with conversions in `rtspcam-pipeline::scale`. macOS needs `Nv12` or
   `Uyvy`/`Yuyv`; Linux browsers handle `Yuyv` and `I420` best (*check*).
5. Tests: the pusher against a fake sink (pacing, stale → placeholder, readers → client
   counts). These run on every OS.

## 4. Phase L — Linux (v4l2loopback)

### L1. Spike (by hand, before writing the backend)

On Ubuntu 24.04 and Debian 12, with `v4l2loopback-dkms` installed:

- Which version each distro ships, and whether it has the control device `/dev/v4l2loopback`
  for adding and removing devices at run time (newer versions, `v4l2loopback-ctl add`;
  *check* the first version that has it).
- Which pixel format the browsers (Chrome, Firefox), Zoom, Teams-in-browser, Discord, OBS and
  `guvcview`/`cheese` accept: write `Yuyv` and `I420` with `ffmpeg -f v4l2` and compare.
  `exclusive_caps=1` is needed for Chrome to list the device (*check*).
- Whether the writer can tell when an app opens the device (v4l2loopback's client-usage event
  in newer versions, *check*). That decides between real on-demand (G3) and "runs while the
  camera is on".
- Who can open `/dev/videoN` (logind normally gives the active session's user access through
  `uaccess`) and the control device (root only by default).
- Secure Boot: whether the DKMS module loads after the distro's MOK enrollment prompt.

### L2. Backend `V4l2Loopback` in `platform/src/linux/`

- `check()`: fails with a clear `Failed` message when the module isn't loaded or can't be
  loaded ("Install v4l2loopback-dkms; see Help > Virtual cameras on Linux"), so the status
  explains what's missing instead of "not supported".
- `create(spec)`: adds a device labelled `spec.name` through the control device (or, if the
  distro's version has none, takes a free device made at boot, §5), sets the output format
  (`VIDIOC_S_FMT`, `spec.preferred`, the format chosen in L1), and starts a `Pusher` whose
  sink `write()`s each frame to the device.
- `remove(id)` / `remove_all()`: stop the pusher, close the file and remove the device if we
  added it.
- `linux::camera_backend()` returns it, and `UnsupportedCameras` if `check()` reports the
  module missing, so the status stays "not supported yet" there.
- ioctls with `nix` or a few `libc` calls; no new C dependency.

### L3. Packaging (`.deb` first, then `.rpm`)

There are no Linux release packages yet (`release.yml` builds only the Windows installer). This
phase adds them; §5 covers the dependency question.

- `cargo-deb` for `.deb`, `cargo-generate-rpm` for `.rpm`; both from the release build in
  `release.yml` on `ubuntu-latest`, attached to the GitHub release like the Windows installer.
- Files: `/usr/bin/rtspcam`, a `.desktop` file and icon, `/etc/modules-load.d/rtspcam.conf`
  (load v4l2loopback at boot), `/etc/modprobe.d/rtspcam.conf` (`exclusive_caps=1`, plus fixed
  devices if L1 shows no control device), and a udev rule if L1 shows the control device
  needs one.
- A `postinst` that runs `modprobe v4l2loopback` once so cameras work without a reboot (and
  says to reboot if that fails, for example Secure Boot waiting for MOK enrollment).

### L4. Verify and document

Each stream as a camera in Chrome, Firefox, Zoom, OBS and `cheese` on Ubuntu (GNOME,
Wayland) and one KDE distro; on-demand behaviour; remove and re-create; two copies of the app;
Secure Boot on. README "What works where" and a progress record.

## 5. Can the Linux release install v4l2loopback automatically?

**Yes, for distro packages.** A `.deb` or `.rpm` can name `v4l2loopback-dkms` as a
dependency, and the package manager installs it with the app. Recommendation:

| Package | Dependency line | Why |
|---|---|---|
| `.deb` (Debian, Ubuntu, Mint, Pop!_OS) | `Recommends: v4l2loopback-dkms` | apt installs Recommends by default, so users get it automatically. Unlike `Depends`, the app can still be installed without it: on Ubuntu the package is in `universe` (*check* that it's enabled by default on desktop installs), and some users can't use DKMS at all. The app works without cameras, so a hard dependency would block installs for no gain. |
| `.rpm` (Fedora, openSUSE) | `Recommends: v4l2loopback` (weak) | On Fedora the module is in RPM Fusion (`akmod-v4l2loopback`), not the main repos (*check*), so a hard `Requires` would make the package uninstallable for most users. README says to enable RPM Fusion. |
| Arch | AUR `PKGBUILD` with `depends=(v4l2loopback-dkms)` | In Arch's `extra` repo (*check*); can be a hard dependency there. Later, community-maintained. |
| AppImage / tarball | none possible | Can't install a kernel module; `check()` explains what to install. |

Things the package can't solve, which the app must explain (status text plus a Help page):

- **Kernel headers.** DKMS builds the module against the running kernel's headers. Ubuntu and
  Debian desktops usually have them through the kernel metapackage; minimal installs may not.
- **Secure Boot.** An unsigned DKMS module only loads after the user enrols a key (MOK) at
  the next boot. Ubuntu's DKMS asks for a password during install and shows the enrolment
  screen at reboot; the app can't do this for them.
- **Kernel updates.** DKMS rebuilds the module for each new kernel; if that fails, the next
  boot has no cameras until it's fixed.

## 6. Phase M — macOS (CoreMediaIO Camera Extension)

### M0. Prerequisites (decisions for the owner, not code)

- An Apple Developer Program membership ($99/year) for a Developer ID certificate,
  notarization, and the `com.apple.developer.system-extension.install` entitlement.
- Minimum macOS: 13 (what OBS requires; camera extensions exist since 12.3).
- The app has to ship as `RtspCam.app` (a bundle), installed in `/Applications`: macOS only
  activates system extensions from there.

### M1. App bundle and DMG (no extension yet)

Package today's binary as `RtspCam.app` (Info.plist, icon, `LSUIElement` off, a universal or
arm64 binary) in a signed, notarized DMG, built in `release.yml` on `macos-latest`. This
is useful on its own (users can run the app without a terminal) and every later step needs it.
Login items (`~/Library/LaunchAgents`) then point at the bundle.

### M2. The camera extension (Swift, Xcode target in `macos/RtspCamCamera/`)

- A `CMIOExtensionProvider` with one device per stream. Each device has a **source** stream
  (what Zoom/FaceTime read) and a **sink** stream (what the app writes to). The extension
  just moves buffers from sink to source; it has no network or decoding code.
- Devices are added and removed at run time through a custom provider property that the app
  sets (id + name); *check* that renaming and removing devices while apps have them open
  works.
- The source stream reports when a client starts or stops it; the extension exposes that as
  a device property, so the app can drive on-demand (G3) like on Windows.
- Pixel format: `NV12` (420v), the format CoreMedia and the pipeline both use natively, at
  the formats `spec.preferred` plus the usual 1080p/720p/480p list.
- Built by `xcodebuild` from a script that `release.yml` calls; embedded at
  `RtspCam.app/Contents/Library/SystemExtensions/`.

### M3. Backend `CmioCameras` in `platform/src/macos/`

- `check()`: asks `OSSystemExtensionManager` to activate the extension (first run: macOS
  shows "System Extension Blocked", the user allows it in System Settings > Privacy &
  Security). Until it's allowed, every camera shows `Failed("Allow the RTSP Cam camera in
  System Settings > Privacy & Security")`.
- `create(spec)`: sets the provider property to add the device, finds it with the
  CoreMediaIO C API (`CMIOObjectGetPropertyData`), opens its sink stream's buffer queue and
  starts a `Pusher` whose sink wraps each frame in a `CVPixelBuffer`/`CMSampleBuffer` and
  enqueues it.
- Rust bindings: the `objc2` family of crates (`objc2-core-media`, `objc2-core-video`,
  `objc2-system-extensions`, CoreMediaIO through a small `extern "C"` block if no binding
  crate covers it; *check* which exist).
- No socket or XPC: the sink stream is Apple's supported way for an app to feed its own
  camera extension, and it works with the extension's sandbox.

### M4. Verify and document

FaceTime, Photo Booth, Zoom, Teams, Chrome, Safari and OBS on macOS 13, 14 and 15; first-run
approval; uninstall (dragging the app to the Trash should remove the extension, *check*);
two cameras at once; on-demand. README and a progress record.

Development without a paid account: `systemextensionsctl developer on` (needs SIP partly
off) lets an unsigned extension load on a test Mac only. That's enough for M2–M3 but not for
release.

## 7. Order, size, and what each phase delivers

| Phase | Size | Delivers | Blocked by |
|---|---|---|---|
| G. Groundwork | small | pusher, placeholder in ipc, new pixel formats; no visible change | — |
| L1–L2. Linux backend | medium | cameras on Linux for anyone who installed v4l2loopback by hand | G |
| L3. Linux packages | small–medium | `.deb`/`.rpm` that pull v4l2loopback in automatically | L2 |
| M1. macOS bundle + DMG | small | a normal Mac app to download | Apple account (signing, notarization) |
| M2–M3. macOS extension + backend | large | cameras on macOS | M1, G, system-extension entitlement |

Linux first is the cheaper and lower-risk path: no account, no new language, and the backend
is one Rust type. macOS can start with M1 in parallel, since it's useful without cameras.

## 8. Risks

| Risk | Mitigation |
|---|---|
| v4l2loopback versions differ per distro (control device, events, formats) | L1 checks the two main distros first; the backend falls back to boot-time devices and "runs while on" when a feature is missing. |
| Some Linux apps reject a format | L1 tries them; the pusher can write whichever format a device is set to. |
| Secure Boot / missing headers leave users without cameras | Clear status text and a Help page; the app keeps working without cameras. |
| Apple doesn't grant the system-extension entitlement or changes the rules | M1 is useful on its own; M2 is the only Apple-dependent step. |
| Extension sandboxing blocks something we need | The design keeps the extension a passive sink-to-source relay; everything else stays in the app. |
| Keeping the app's frame loop and the DLL's in step | Both use the same placeholder (G1); the pusher mirrors `deliver()`'s pacing and staleness rules, and the tests pin them. |
