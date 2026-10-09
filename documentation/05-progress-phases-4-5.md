# 05 — Progress: Phases 4 and 5

Status as of 2026-10-09. Continues [03-progress-phases-2-3.md](03-progress-phases-2-3.md) and
[04-session-2026-10-09.md](04-session-2026-10-09.md) against [01-plan.md](01-plan.md).

## 1. Summary

| Phase | Status |
|---|---|
| 4 Host integration (CameraManager) | **Code done and tested** against a fake Windows backend and the Docker RTSP server. The exit criteria that involve real apps (Discord switching cameras, Quit while Discord streams) are **not verified**: they need the admin install of the DLL, which has still not been done. |
| 5 Desktop UI | **Done and run for real**: window, list, live preview, dialogs, tray, minimize to tray, quit. Screenshots were checked by eye. Exit criterion "use it in Discord" has the same blocker as Phase 4. |

Numbering note: the Phase 0 decision record, if written, is now `06`.

## 2. Phase 4

| Plan item | Status | Where |
|---|---|---|
| `CameraManager`: pipeline + IPC server + virtual camera per enabled stream, reacts to config changes | Done | `rtspcam-app/src/manager.rs` |
| On-demand: consumer connect starts RTSP, last one leaving stops it after a grace period | Done (grace 10 s by default, configurable) | `manager.rs` (`supervise`) |
| Format negotiation | Already in the pipe protocol (Phase 3); `CameraSource` scales to what each consumer asked for | `source.rs` |
| Health/status model for the UI | Done: `CameraStatus { activity, vcam, clients, previews }` | `status.rs` |
| Single-instance guard; second launch brings the window forward | Done (named mutex + event) | `single_instance.rs` |
| Quit = process exit: ordered shutdown, 3 s watchdog | Done | `CameraManager::shutdown` |
| Startup cleanup of stale cameras | Done, **unverified** (see below) | `backend.rs` (`remove_stale`) |

### How it works

- `apply(config)` never blocks. A reconciler task diffs the config against the running cameras,
  removes what disappeared or changed (any change to a stream rebuilds that camera), and adds what
  is new. Slow Windows calls run on a dedicated thread (`VcamBackend`) that owns Media Foundation
  and every `IMFVirtualCamera`.
- Each camera has a supervisor task. Demand = apps using the camera + open previews. On-demand
  streams connect when demand > 0 and disconnect after the grace. "Pause all" disconnects at once
  and makes apps show "Camera disabled".
- Shutdown order: stop config changes and the pipe servers, remove every camera, stop the
  pipelines (joining decode threads), stop the runtime. A watchdog thread ends the process after
  3 s regardless. Measured: quitting the GUI takes about 40 ms with no consumers.
- `CameraBackend` is a trait so the manager is tested without the DLL.

### Changes outside the app crate

- **`rtspcam-ipc` server**: connected clients are now tracked in a `JoinSet`, so aborting the
  server also closes its client pipes. Before, a rebuilt camera could not re-create its pipe
  (`first_pipe_instance`) while an old client was still attached.
- **Do not use `Path::exists` on a pipe**: it opens the pipe and takes the instance away from a
  real client (the test hit `ERROR_PIPE_BUSY`). `pipe_exists` uses `WaitNamedPipeW` instead.

### Tests (`rtspcam-app/tests`)

Always run: cameras only for enabled streams, creation failures reported, unsupported protocol
shown as an error, edits rebuild only the changed camera and removals remove, shutdown removes
cameras and pipes promptly with a consumer attached, pause/resume, an unreachable camera,
single instance. With `RTSPCAM_TEST_SERVER`: on-demand connect and disconnect with grace,
preview keeps a stream connected, always-on streams connect alone.

## 3. Phase 5

| Plan item | Status |
|---|---|
| Main window: buttons, menu (File: Settings, Quit; Help: logs folder, About), list view, status bar | Done |
| Preview pane with codec/size/fps/bitrate/latency text | Done, **GDI** (`StretchDIBits`), no Direct2D |
| Add/Edit dialog with all §3.2 fields, protocol combo, masked password with show toggle | Done |
| Paste full URL (other schemes rejected clearly) | Done ("Fill" button) |
| Inline validation, OK disabled until valid, error text | Done (first two problems under the form) |
| Test connection with codec/resolution/thumbnail or a friendly error | Done (15 s limit) |
| Remove with confirmation (also removes the camera) | Done |
| Tray icon: tooltip, double-click restores, menu Open / Pause all / Quit, `TaskbarCreated` | Done |
| Minimize to tray (D6); Close (X) quits | Done, checked live |
| Settings: minimize to tray, start with Windows (`--minimized`), log level | Done |
| Friendly errors, Discord tip after adding | Done (error hints from `ErrorKind`; one tip per session) |
| "Enabled" checkbox column | **Deviation**: text column "Yes/No"; the Start/Stop button toggles it |

Verified by running the real exe on this machine: the window opens with the three test streams,
the first row is selected and its preview shows the live H.264 stream with stats; the Add dialog
opens and renders; minimize hides the window; a second launch brings it back; WM_CLOSE ends the
process (exit code 0, no process left). The DLL is not installed, so each camera correctly shows
"Camera error: Access is denied ..." (now with a hint to install the media source).

Also added: `--headless` mode (manager only, Ctrl+C quits, live config reload), an embedded
manifest (Common Controls v6, PerMonitorV2, asInvoker) via the linker, a runtime-drawn app icon,
and the autostart Run-key helper (tested against a throwaway value name).

### Things worth knowing

- `DrawTextW` with an empty buffer crashed the process with `0xC000041D` (an exception inside a
  window callback, nothing in the log). It is guarded now; any future "exits right after the
  window appears with no log" is probably a native exception in a paint or message handler.
- `winsafe` 0.0.29 needs Rust 1.87 while the workspace declares 1.85; the pinned toolchain is
  stable so it builds. Raising `rust-version` makes newer MSRV-gated clippy lints fire in older
  crates, so it was left alone.
- `winsafe` has no icon-creation or `StretchDIBits` wrappers; those and the tray use the
  `windows` crate.
- Positions in dialogs are in 96-dpi units and winsafe scales them by the system DPI.
- The UI polls (500 ms status, 40 ms preview) instead of receiving change notifications. Simple
  and cheap; `ManagerOptions::on_change` exists if that ever matters.

## 4. Not verified, needs a person or the admin install

1. Install the DLL (`tools/vcam/install-dev.ps1`), then the Frame Server checks from §6 of
   [04](04-session-2026-10-09.md): camera id reaches the source (S0.2), process exit removes the
   cameras (S0.6), cameras in Camera app / Chrome / OBS / Discord.
2. Phase 4/5 exit criteria with real apps: three cameras selectable in Discord, quitting while
   Discord uses a camera leaves no `rtspcam.exe`, Unplug/replug shows "No signal" then recovers
   (the pipeline side is tested; the DLL's placeholder side was tested in Phase 3).
3. `remove_stale` only removes cameras whose pipe no longer exists, by recreating them with the
   name Media Foundation lists. Whether that name round-trips is unknown until Frame Server runs.
4. Everything is still unpushed and CI has never run.

## 5. Next

Phase 6 (discovery, brand templates, import/export), Phase 7 (installer, signing, OS check in the
installer), Phase 8 (GPU path, soak, fuzzing, crash reports). Before those, do item 1 above.
