# 09 — Plan: RTSP Cam on Windows, Linux and macOS

Status: plan, written before any code changed. The record of what was done is
[10-progress-cross-platform.md](10-progress-cross-platform.md).

## 1. Goal and limits

1. Everything that is not inherently tied to one OS builds and is tested on Windows, Linux and
   macOS.
2. OS-specific code sits behind small traits with one implementation per OS, all in one crate
   (`rtspcam-platform`), so the native boundary is obvious from the crate layout.
3. The winsafe (Win32) interface is replaced by a Slint interface that runs on all three.
4. Windows keeps working as today: same DLL, same config file, same pipe protocol, same
   behaviour.

Out of scope: the macOS CoreMediaIO Camera Extension and the Linux v4l2loopback backend. Both
OSes get a stub camera backend that reports "Virtual cameras are not supported on this
platform yet", so preview, config and discovery work there without cameras.

Work happens on the branch `feat/cross-platform` (it already existed, identical to `master`;
the brief named it `cross-platform`).

## 2. Where the code disagrees with the brief

Checked against the code and the current crate versions on 2026-10-09.

| Brief says | Actually | What this plan does |
|---|---|---|
| Slint has no tray API; use `tray-icon` + `muda`. | Slint 1.18 has a built-in `SystemTrayIcon` element (default feature `system-tray`): `Shell_NotifyIconW` on Windows (re-added after an Explorer restart), AppKit `NSStatusItem` on macOS, `ksni` (StatusNotifierItem over D-Bus) on Linux. | Use Slint's tray. No `tray-icon`, no `muda`, no GTK, no second event loop, no main-thread juggling on macOS. On Linux the tray needs a StatusNotifierItem host (KDE, most desktops; GNOME needs the AppIndicator extension), which is the same requirement `tray-icon` has through libappindicator. Loss: Slint's tray has no balloon notification, so the one-time "still running in the tray" balloon is dropped. Left-click (not double-click) opens the window, because that is what Slint reports. |
| `core` is nearly portable; `secret.rs` has a non-Windows branch. | `Secret` encrypts inside its `serde` impls, so `core` itself must reach the OS store, and the dependency arrow points from `platform` to `core`. | The `SecretStore` trait lives in `core` (next to the `serde` code that calls it) and is installed once at startup; every implementation is in `rtspcam-platform`, which re-exports the trait. See §4.2. |
| `config/store.rs` uses `ReplaceFileW`. | Yes, inside `ConfigStore::save`. | A `FileReplace` trait in `core` with a portable default (copy to `.bak`, then rename); the Windows `ReplaceFileW` version is in `platform` and passed to `ConfigStore`. Windows saves stay identical. |
| `rtspcam-pipeline`: "MF decoder is `cfg(windows)`, optional on Windows". | True, but the boundary rule forbids OS `cfg`s in the pipeline. | The pipeline gets a `PlatformDecoders` hook. The Media Foundation decoder moves to `platform/windows/` and is installed at startup. The decoder choice `mf` becomes `platform` (`mf` is still accepted on the CLI). |
| `rtspcam-ipc`: portable protocol + Windows client/server. | The server creates the named pipes (with their DACL) itself; the client opens `\\.\pipe\...` as a file. | `ipc` keeps the protocol, a generic blocking client over any `Read + Write`, and a generic async server over a `FrameListener` trait. The named-pipe listener moves to `platform/windows/`; Unix sockets go to `platform/unix/`. The wire protocol does not change. |
| The engine runs the pipe server; backends only create cameras. | That only fits the Windows model. | A camera backend receives the camera's `FrameSource` and delivers frames however its OS needs: Windows serves it over a pipe to the DLL; a future v4l2loopback backend would pull frames and write them to a device; a future macOS extension would be served over a socket. See §4.1. |
| `overlay.rs`: check what it uses. | GDI (`CreateFontW` "Segoe UI", `TextOutW`) and `GetLocalTime`. | `ab_glyph` with an embedded Inter font (OFL 1.1, the same file Slint ships) and `chrono` for local time. Text looks slightly different from Segoe UI on Windows. |
| Not mentioned | `rtspcam-cli/src/rtsp.rs` has a `cfg(windows)` memory reader; `cli/src/vcam.rs` is all Windows. | Memory reading moves to `platform`. The `vcam` subcommand stays Windows-only (gated in the CLI, as the brief allows). |
| Not mentioned | Slint 1.18 needs Rust 1.92; the workspace says 1.85. | Raise `rust-version` to 1.92 and fix whatever MSRV-gated clippy lints appear. Also allows `std::fs::File::try_lock` (1.89) for the Unix single-instance lock. |
| Not mentioned | Slint has no modal or owned windows (no parent/transient API in its winit backend), and Wayland does not let apps position windows. | Dialogs are drawn as modal sheets inside the main window (the main window's content is blocked while one is open). Each sheet keeps today's fields, order and buttons. |

No change is needed to the IPC wire protocol, the config file format or the DLL's COM
behaviour.

## 3. Crate layout

```
crates/
  rtspcam-core        portable: config, validation, paths, logging, constants, Secret,
                      SecretStore trait, FileReplace trait (+ portable default)
  rtspcam-ipc         portable: protocol, FrameSource trait, blocking FrameClient<S>,
                      async serve() over a FrameListener (feature "server")
  rtspcam-pipeline    portable: RTSP (retina), OpenH264, scaling, frames, frame bus;
                      PlatformDecoders hook
  rtspcam-onvif       portable (unchanged)
  rtspcam-platform    the native boundary; every OS cfg lives here
    src/lib.rs          traits, re-exports, constructors (native backend per OS), install()
    src/camera.rs       VirtualCameraBackend, CameraSpec, CameraError, UnsupportedCameras
    src/autostart.rs    Autostart trait
    src/instance.rs     SingleInstance trait, Instance, InstanceLock
    src/transport.rs    FrameTransport trait
    src/desktop.rs      open_folder, attach_parent_console, process_memory
    src/windows/        MF virtual cameras (via rtspcam-vcam-mgr), DPAPI, Run key,
                        named mutex + event, named pipes + DACL, ReplaceFileW,
                        Media Foundation decoder
    src/unix/           shared by Linux and macOS: keyring-backed secret store,
                        lock file + socket single instance, Unix socket transport
    src/linux/          stub cameras, XDG autostart (~/.config/autostart/*.desktop)
    src/macos/          stub cameras, LaunchAgent (~/Library/LaunchAgents/*.plist)
  rtspcam-engine      portable: CameraManager, Preview, status, form model, overlay,
                      connection test, ONVIF helpers (today's rtspcam-app minus UI and OS code)
  rtspcam-vcam        Windows-only DLL; only import paths change
  rtspcam-vcam-mgr    Windows-only, unchanged; used by platform/windows and the CLI
  rtspcam-app         Slint UI + main(); depends on engine + platform
  rtspcam-cli         portable; the `vcam` subcommand is Windows-only
```

Dependencies (arrows point at what is used):

```
app ──► engine ──► platform ──► pipeline ──► core
 │        │          │  └──► ipc ─────────────┘
 │        │          └──► vcam-mgr (Windows only)
 │        └──► onvif
 └──► platform
vcam (DLL) ──► ipc, core          (no tokio, no platform)
```

The engine depends on `rtspcam-platform` only for the trait definitions. It never calls the
per-OS constructors; `main()` does, and hands the results to the engine.

### Where `cfg` may appear

- `rtspcam-platform` (all of it), `rtspcam-vcam` and `rtspcam-vcam-mgr` (Windows-only crates).
- `rtspcam-cli`: the `vcam` subcommand only.
- Packaging, not behaviour: `#![windows_subsystem = "windows"]` in `app/src/main.rs` (ignored on
  other OSes) and the build scripts that embed the Windows manifest and version info (they read
  `CARGO_CFG_TARGET_ENV`).
- `cfg(feature = ...)` and `cfg(test)` are fine everywhere.

`core`, `ipc`, `pipeline`, `onvif`, `engine` and the UI have no OS `cfg`s. A grep in CI keeps
it that way (§6 step E).

## 4. Traits

Each trait lives with the code that calls it. Where that is not `rtspcam-platform`, the
platform crate re-exports it, so `rtspcam_platform::*` is the one place to find all of them.

### 4.1 Virtual cameras — `rtspcam_platform::camera`

```rust
/// What a camera should look like to apps.
pub struct CameraSpec {
    pub id: Uuid,          // the stream id
    pub name: String,      // shown in camera pickers
    pub preferred: (u32, u32, u32), // (width, height, fps) to list first
}

pub enum CameraError {
    /// This OS has no virtual camera backend yet. Shown as a status, not as an error.
    Unsupported(String),
    /// The backend exists but this camera could not be made (not installed, access denied,
    /// Windows 10, ...). The text is shown to the user.
    Failed(String),
}

/// Makes virtual cameras and gets frames to them.
///
/// How each OS implements it:
/// - Windows: `MFCreateVirtualCamera` with session lifetime. Frame Server loads
///   rtspcam_vcam.dll, which connects to a named pipe; `create` serves `frames` on that pipe.
/// - Linux (future): v4l2loopback. `create` opens /dev/videoN and starts a thread that pulls
///   `frames` at the camera's rate and writes them to the device (push; no consumer process).
/// - macOS (future): a CoreMediaIO Camera Extension. `create` asks the extension for a device
///   and serves `frames` to it over a socket or XPC (serve-to-consumer, like Windows).
/// - Today on Linux and macOS: `UnsupportedCameras`, which returns `Unsupported`.
pub trait VirtualCameraBackend: Send + Sync + 'static {
    /// Whether this computer can have virtual cameras at all.
    fn check(&self) -> Result<(), CameraError>;
    /// Creates (or re-creates) the camera and starts delivering `frames` to it. Blocking;
    /// called on the engine's blocking pool, never the UI thread. `runtime` is the engine's
    /// tokio runtime, for backends that serve frames asynchronously.
    fn create(&self, spec: &CameraSpec, frames: Arc<dyn FrameSource>, runtime: &Handle)
        -> Result<(), CameraError>;
    /// Removes one camera and stops its frame delivery (no-op if unknown). Blocking.
    fn remove(&self, id: Uuid);
    /// Removes every camera and releases whatever the backend holds. Blocking.
    fn remove_all(&self);
}
```

`FrameSource` is today's trait from `rtspcam-ipc` (moved out of the `server` module so it needs
no feature): "a consumer connected with format F", "it left", "write the newest picture in
format F if newer than sequence N", "current status". A push backend calls the same methods
itself: `client_connected` once, then `next_frame` at the device's frame rate.

A v4l2loopback backend therefore means one new type in `platform/src/linux/` implementing this
trait, and changing `linux::camera_backend()` to return it. Nothing else.

### 4.2 Secrets — `rtspcam_core::secret`, re-exported by platform

```rust
/// Encrypts passwords for config.json.
///
/// - Windows: DPAPI, current user, values stored as `dpapi:<base64>` (unchanged).
/// - Linux and macOS: a random 256-bit key kept in the OS keyring (Secret Service on Linux,
///   the login Keychain on macOS) through `keyring-core`; each value is ChaCha20-Poly1305
///   encrypted with it and stored as `keyring:<base64>`. If no keyring is reachable (a
///   headless Linux box without Secret Service), the key is kept in a file readable only by
///   the user next to config.json, and values are stored as `keyfile:<base64>`.
pub trait SecretStore: Send + Sync + 'static {
    /// Encrypts `plain`; returns the stored form including its prefix.
    fn seal(&self, plain: &str) -> Result<String, SecretError>;
    /// Decrypts a stored value that starts with one of `SEALED_PREFIXES`.
    fn unseal(&self, stored: &str) -> Result<Zeroizing<String>, SecretError>;
}

pub const SEALED_PREFIXES: [&str; 3] = ["dpapi:", "keyring:", "keyfile:"];
pub fn install_store(store: Box<dyn SecretStore>);   // once, at startup
```

Why the keyring crate, and why only for one key: `keyring-core` (keyring v4) is the maintained
cross-platform wrapper for Secret Service and Keychain, pure Rust over D-Bus on Linux. Keeping
one key there (instead of one keyring entry per password) keeps config.json self-contained like
DPAPI does: no orphaned keyring entries when streams are deleted, and export/import behave the
same. The key file fallback means a password can always be saved, at the cost of protection
equal to the file's permissions; this is logged as a warning.

Failure handling stays as today: a value that cannot be decrypted (another user, another
machine, a `dpapi:` value on Linux, a missing keyring key) becomes a "locked" secret. It is kept
in the file unchanged, the stream shows the existing "PasswordLocked" problem, and the edit
dialog shows an empty password box, so the user re-enters it. Nothing crashes. A plain-text
value typed into the file by hand is encrypted on the next save, as today.

### 4.3 Saving files — `rtspcam_core::config::FileReplace`, re-exported by platform

```rust
/// Moves `tmp` over `target`, keeping the old `target` as `backup`.
/// - Windows: `ReplaceFileW` (keeps ACLs and attributes), falling back to the default.
/// - Linux and macOS: the default, `CopyThenRename` (copy to .bak, then rename, which is
///   atomic on the same file system).
pub trait FileReplace: Send + Sync + fmt::Debug {
    fn replace(&self, tmp: &Path, target: &Path, backup: &Path) -> io::Result<()>;
}
impl ConfigStore { pub fn with_replacer(self, replacer: Arc<dyn FileReplace>) -> Self; }
```

### 4.4 Start at login — `rtspcam_platform::Autostart`

```rust
/// Starts the app (with `--minimized`) when the user signs in.
/// - Windows: value "RtspCam" in HKCU\...\CurrentVersion\Run (unchanged).
/// - Linux: ~/.config/autostart/rtspcam.desktop (XDG Autostart).
/// - macOS: ~/Library/LaunchAgents/io.github.carl-eis.rtspcam.plist with RunAtLoad.
pub trait Autostart: Send + Sync {
    /// The setting's name in the UI: "Start with Windows" or "Start at login".
    fn label(&self) -> &'static str;
    fn is_enabled(&self) -> bool;
    fn set(&self, enabled: bool) -> io::Result<()>;
}
```

The config field keeps its name, `start_with_windows`.

### 4.5 One copy per user — `rtspcam_platform::SingleInstance`

```rust
/// - Windows: named mutex `Local\RtspCam.SingleInstance` and event `Local\RtspCam.Show`
///   (unchanged).
/// - Linux and macOS: an exclusive lock on `<runtime dir>/RtspCam.lock` (released by the OS
///   if the process dies) and a Unix socket next to it; a second copy connects and asks the
///   first to show itself.
pub trait SingleInstance: Send + Sync {
    fn acquire(&self) -> io::Result<Instance>;
}
pub enum Instance { First(Box<dyn InstanceLock>), AlreadyRunning }
pub trait InstanceLock: Send + fmt::Debug {
    /// Calls `on_show` (on a background thread) whenever another copy starts.
    fn on_show(&mut self, on_show: Box<dyn Fn() + Send>);
}
```

The constructor takes the name (`"RtspCam"` in the app), so tests can use their own.

### 4.6 Frame transport — `rtspcam_platform::FrameTransport` and `rtspcam_ipc::server::FrameListener`

```rust
// rtspcam-ipc (feature "server"): what serve() needs.
pub trait FrameListener: Send {
    /// Waits for the next consumer.
    fn accept(&mut self) -> Pin<Box<dyn Future<Output = io::Result<BoxedConnection>> + Send + '_>>;
}
pub async fn serve(listener: Box<dyn FrameListener>, source: Arc<dyn FrameSource>) -> io::Result<()>;

// rtspcam-platform: endpoints per camera.
/// - Windows: named pipe \\.\pipe\rtspcam\<id> with the DACL for SYSTEM, LOCAL SERVICE
///   and the owner (unchanged).
/// - Linux and macOS: Unix socket <runtime dir>/rtspcam/<id>.sock in a 0700 folder.
pub trait FrameTransport: Send + Sync {
    /// Starts serving camera `id`. Fails if something already serves it. Needs a tokio runtime.
    fn listen(&self, id: Uuid) -> io::Result<Box<dyn FrameListener>>;
    /// Connects as a consumer (tests and tools; the DLL opens the pipe itself).
    fn connect(&self, id: Uuid) -> io::Result<Box<dyn ReadWrite>>;
    /// Whether something serves camera `id`, checked without taking a connection
    /// (Windows: WaitNamedPipeW, never Path::exists).
    fn is_served(&self, id: Uuid) -> bool;
}
```

The DLL keeps opening the pipe through `FrameClient::open(frame_pipe_name(id), format)`, a
portable "open a file-system path" constructor.

### 4.7 Platform decoders — `rtspcam_pipeline::decode::PlatformDecoders`, re-exported

```rust
/// - Windows: Media Foundation decoder MFTs (H.264, H.265, MJPEG) — today's MfDecoder.
/// - Linux and macOS: none yet; OpenH264 decodes H.264. (VideoToolbox / VA-API could be
///   added here later.)
pub trait PlatformDecoders: Send + Sync + 'static {
    fn create(&self, codec: VideoCodec, size: Option<(u32, u32)>)
        -> Result<Box<dyn Decoder>, PipelineError>;
    /// Advice when a codec cannot be decoded (Windows: the HEVC Store extension).
    fn unavailable_hint(&self) -> Option<&'static str>;
}
pub fn install_platform_decoders(decoders: Box<dyn PlatformDecoders>);
```

### 4.8 Small helpers — `rtspcam_platform::desktop` (functions, not traits)

`open_folder(path)` (Explorer / `open` / `xdg-open`), `attach_parent_console()` (Windows only;
no-op elsewhere), `process_memory()` (working set and private bytes; `None` where not
implemented).

### 4.9 Startup

```rust
// main()
rtspcam_platform::install();          // secret store + platform decoders
let backend = rtspcam_platform::camera_backend();
match backend.check() { Err(CameraError::Failed(m)) => fatal(m), _ => {} }  // Windows 10
let instance = rtspcam_platform::single_instance("RtspCam").acquire()?;
let store = ConfigStore::open_default()?.with_replacer(rtspcam_platform::file_replacer());
let manager = CameraManager::start(backend, options)?;
```

## 5. Where each file goes

| Today | After |
|---|---|
| `core/src/secret.rs` (DPAPI module) | `platform/src/windows/dpapi.rs`; `Secret` and the store hook stay in core |
| `core/src/config/store.rs` (`replace_file`) | `platform/src/windows/files.rs`; trait + default stay in core |
| `core/src/paths.rs` | unchanged code; docs list each OS's folders |
| `ipc/src/server.rs` (pipe creation, `security` module) | `platform/src/windows/pipe.rs`; generic `serve` stays in ipc |
| `ipc/src/client.rs` | stays, generic over the stream; pipe error codes move to the DLL |
| `ipc/tests/pipe.rs` | `platform/tests/transport.rs` (runs on every OS) |
| `pipeline/src/decode/mf.rs` | `platform/src/windows/mf_decoder.rs` |
| MF cases in `pipeline/tests/decode.rs`, `pipeline.rs` | `platform/tests/` (Windows only) |
| `app/src/backend.rs` | trait → `platform/src/camera.rs`; `VcamBackend` → `platform/src/windows/camera.rs` |
| `app/src/autostart.rs` | `platform/src/windows/autostart.rs` |
| `app/src/single_instance.rs`, `app/tests/single_instance.rs` | `platform/src/windows/instance.rs`, `platform/tests/single_instance.rs` |
| `app/src/manager.rs`, `source.rs`, `status.rs`, `form.rs` | `engine/src/` |
| `app/src/overlay.rs` | `engine/src/overlay.rs`, GDI replaced by `ab_glyph` |
| `run_test` in `app/src/ui/stream_dialog.rs` | `engine/src/probe.rs` |
| ONVIF address parsing in `discover_dialog.rs` | `engine/src/discovery.rs` |
| `app/tests/manager.rs` | `engine/tests/manager.rs` (fake backend; no pipes needed) |
| `app/src/ui/*` (winsafe) | deleted; replaced by `app/ui/*.slint` + `app/src/ui/*.rs` |
| `cli/src/rtsp.rs` `memory()` | `platform::desktop::process_memory()` |

## 6. Steps (each leaves Windows green and is committed)

**A. Extract `rtspcam-platform` (pure move).** Create the crate with the traits and the
Windows implementations moved from core, ipc, pipeline and app. Add the hooks to core
(`SecretStore`, `FileReplace`), ipc (`FrameListener`, generic client) and pipeline
(`PlatformDecoders`). The engine-side change of who serves the pipe happens here too, inside
the Windows backend. `rtspcam-app` (still winsafe) and the CLI call `rtspcam_platform::install()`
and the constructors. Check: build, clippy, all tests on Windows.

**B. Extract `rtspcam-engine`.** Move manager, source, status, form and overlay; add `probe`
and `discovery`. Overlay text through `ab_glyph`. The winsafe UI uses the engine for now. Move
the CLI's memory reader. No OS `cfg` remains outside platform (plus the listed exceptions).

**C. Linux and macOS modules.** `unix/` (keyring secrets with key-file fallback, lock-file
instance, Unix socket transport), `linux/` and `macos/` (stub cameras, autostart). Verify on
Linux in a Docker container (build, clippy, tests); `cargo check` for macOS what can be checked
from Windows without a C cross compiler.

**D. Slint UI.** `.slint` files compiled by `slint-build`; Rust controllers per window; preview
fed by a worker thread through `SharedPixelBuffer`; Slint's tray; `rfd` for file dialogs.
Delete the winsafe UI and the `winsafe` dependency.

**E. CI.** Jobs for `windows-latest` (as today plus the installer), `ubuntu-latest` and
`macos-latest` (fmt, clippy `-D warnings`, tests). A step that fails if an OS `cfg` appears
outside the allowed places. Drop the target pin from `rust-toolchain.toml`.

**F. Docs.** README (per-OS build and run, what works where) and
`10-progress-cross-platform.md`.

## 7. Risks

| Risk | Mitigation |
|---|---|
| Behaviour drift on Windows while moving code | Phase A is a move: same functions, same names, same order of operations. The Windows-only tests (DPAPI, Run key, single instance, pipe transport, MF decode) move with the code and keep running. What can only be checked by a person is listed in 10. |
| Slint text rendering or layout differs per OS | Fixed "fluent" style on every OS; layouts use Slint layouts, not fixed pixel positions. |
| Preview cost on the UI thread | Scaling and colour conversion run on a worker; the UI thread only swaps an image, at most at the display rate. |
| Linux tray needs a StatusNotifierItem host | The app works without a tray; a second launch shows the window. Documented. |
| macOS cannot be built here | CI builds and tests it. What the CI job does not exercise (Keychain prompts, LaunchAgent at login, tray) is listed as unverified. |
