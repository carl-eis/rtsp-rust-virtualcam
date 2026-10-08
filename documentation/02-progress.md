# 02 — Progress: what's done so far

Status as of 2026-10-08. This file records what has been built against the plan in
[01-plan.md](01-plan.md), the decisions made along the way, and what is still unverified.

## 1. Summary

| Phase | Status |
|---|---|
| 0 Feasibility spikes | **Not started.** Phase 1 was done first because it doesn't depend on the spike results. The Phase 0 decision record (`02-spike-results.md` in the plan) will take the next free number. |
| 1 Workspace, foundations and CI | **Done**, except that the CI workflow hasn't run on GitHub yet. The test RTSP streams were verified on 2026-10-09 (see [03-progress-phases-2-3.md](03-progress-phases-2-3.md)). |
| 2–3 | See [03-progress-phases-2-3.md](03-progress-phases-2-3.md). |
| 4–8 | Not started. |

## 2. Phase 1 checklist

| Plan item | Status | Where |
|---|---|---|
| Cargo workspace per §3.3 | Done | `Cargo.toml`, `crates/*` |
| `rust-toolchain.toml` (stable, `x86_64-pc-windows-msvc`) | Done | `rust-toolchain.toml` |
| clippy / rustfmt config | Done | `[workspace.lints]` in `Cargo.toml`, `clippy.toml`, `rustfmt.toml` |
| JSON config model per §3.2 | Done | `crates/rtspcam-core/src/config/mod.rs` |
| `Protocol` enum, `default_port()`, `StreamConfig::url()` | Done | `config/protocol.rs`, `config/mod.rs` |
| `AppSettings { minimize_to_tray, start_with_windows, log_level }` | Done | `config/mod.rs` |
| Validation (unique names, host/IP, port range) | Done | `config/validate.rs` |
| DPAPI `Secret` (`dpapi:` base64, redacted `Debug`) | Done | `secret.rs` |
| Atomic save + `.bak`, schema `version` + migrations, file watching | Done | `config/store.rs`, `config/migrate.rs`, `config/watch.rs` |
| Unit tests (round-trip, defaults, URL building, DPAPI) | Done: 36 tests | `#[cfg(test)]` modules in `rtspcam-core` |
| Logging with `tracing` (rolling files) + panic hook | Done | `logging.rs` |
| GitHub Actions on `windows-latest` | Written, **not yet run** | `.github/workflows/ci.yml` |
| `tools/test-rtsp/` with mediamtx + ffmpeg | Written, **not yet run** | `tools/test-rtsp/` |

Also added (not in the plan): `.gitignore`, `README.md`, `mise.toml` tasks, and the
`rtspcam-cli config` commands.

## 3. What exists, crate by crate

### `rtspcam-core`

The only crate with real functionality so far.

- **Config model** (`Config`, `AppSettings`, `StreamConfig`, `OutputFormat`, `Transport`,
  `FitMode`, `LogLevel`).
  - Every field has a default (`#[serde(default)]`), so `{}` is a valid config.
  - A stream with no `id` gets a new UUID.
  - Unknown fields are kept in an `extra` map at each level and written back on save.
    `serde_json`'s `preserve_order` keeps their order.
- **`Protocol`**: `Rtsp`, plus `Unsupported(String)` for values this build doesn't know.
  - An unknown value such as `"http"` loads, is reported by validation, and is saved
    unchanged, so the rest of the config is unaffected (§3.2 rule).
  - Parsing is case-insensitive. `Protocol::SUPPORTED` lists the entries for the UI combo box.
- **`StreamConfig::url()`** builds `rtsp://host:port/path`.
  - IPv6 addresses are bracketed, and an IPv6 zone id's `%` is written as `%25`.
  - A leading `/` is added to the path if missing.
  - Credentials are never included. They're available separately through `credentials()`.
- **Validation**: `Config::validate()` and `StreamConfig::validate()` return a list of
  `ValidationIssue { stream, field, problem }`, so the UI can show each error next to its
  field. It checks:
  - a name is set, and names are unique (ignoring case and surrounding spaces)
  - ids are unique
  - the protocol is supported
  - the host is an IPv4/IPv6 address or a valid host name (`192.168.1.300` is rejected)
  - the port is not 0
  - the path has no spaces or control characters
  - the password can be decrypted
  - the output size and fps are among those the camera offers (1080p/720p/480p × 30/15)
- **`Secret`**: DPAPI with current-user scope and app-specific entropy, stored as
  `dpapi:<base64>`.
  - The plain text is wiped from memory on drop (`zeroize`).
  - `Debug` prints `Secret(<redacted>)`.
  - A plain-text value typed into the file by hand is accepted and encrypted on the next save.
  - A value that can't be decrypted (for example a config copied from another user or PC)
    becomes a *locked* secret: it is written back unchanged and validation flags it.
- **`ConfigStore`**: load and save `%APPDATA%\RtspCam\config.json`.
  - A missing file loads as the default config.
  - Saving writes `config.json.tmp`, then calls `ReplaceFileW`, which keeps the old file as
    `config.json.bak`. If that call fails, it falls back to copy + rename.
- **Migrations**: run on the raw JSON before deserializing.
  - A missing `version` is treated as version 1.
  - A file with a newer version than this build supports is refused, so the app never
    silently downgrades it.
  - There are no migrations yet. A compile-time assert keeps the migration table and
    `CURRENT_VERSION` in step.
- **`ConfigWatcher`** (feature `watch`, uses `notify`):
  - Watches the folder rather than the file, because an atomic replace swaps the file out.
  - Waits 300 ms for events to stop before reloading.
  - Ignores the app's own saves by comparing against the last bytes read or written.
  - Reports invalid JSON as an `Err`, so the app can show the error and keep the last good
    config.
- **Logging** (feature `logging`):
  - Daily log files in `%LOCALAPPDATA%\RtspCam\logs`, keeping 7.
  - Optional output to stderr.
  - The level can be changed while running (`LogGuard::set_level`).
  - `RTSPCAM_LOG` overrides the level using `EnvFilter` syntax.
  - A panic hook logs the message, location, thread and backtrace.
- **Constants**: the virtual camera CLSID `{D533721F-03A1-4D9E-B562-31E82D1ED87F}`
  (generated for this project), and pipe names `\\.\pipe\rtspcam\<stream id>`.
- **Paths**: `config_dir()`, `config_file()`, `log_dir()`.

The virtual camera DLL uses this crate with `default-features = false`, which leaves out
`notify` and the `tracing` subscriber. CI runs clippy on that build to keep it working.

### `rtspcam-app` (`rtspcam.exe`)

Only startup for now:
1. Load the config.
2. Start logging at the configured level.
3. Log any validation problems and each enabled stream's URL.
4. Exit.

No UI, tray icon or cameras yet (Phases 4–5).

### `rtspcam-cli` (`rtspcam-cli.exe`)

- `config path`: print where the config file is.
- `config show`: print the config with passwords replaced by `<redacted>`.
- `config validate`: list problems and exit with status 1 if there are any.
- `config add-test-streams [--host] [--port]`: add the `tools/test-rtsp` streams, including
  credentials for the secure one.
- `--config <file>`: use another config file. `-v`: debug logging.

### Placeholder crates

`rtspcam-ipc`, `rtspcam-pipeline`, `rtspcam-vcam` (a `cdylib` that builds to
`rtspcam_vcam.dll`) and `rtspcam-vcam-mgr` contain only module docs describing their
future role.

### Tooling

- **CI** (`.github/workflows/ci.yml`) runs these steps on `windows-latest`:
  1. `cargo fmt --check`
  2. clippy with `-D warnings`, both for the workspace and for the DLL's no-default-features
     build
  3. tests
  4. release build
  5. upload of `rtspcam.exe`, `rtspcam-cli.exe`, `rtspcam_vcam.dll` and the `.pdb` files

  All cargo commands use `--locked`, so `Cargo.lock` is committed.
- **Test RTSP server** (`tools/test-rtsp/`):
  - `mediamtx.yml` serves five ffmpeg test-pattern streams on port 8554: H.264 720p,
    H.264 1080p, H.265 720p, MJPEG 720p, and an H.264 stream that requires the login
    `rtspcam` / `rtspcam-test`.
  - `start.ps1` runs it natively or with `-Docker`.
  - `probe.ps1` checks each stream with ffprobe, including that the secure one refuses
    clients without credentials.
- **mise tasks**: `fmt`, `lint`, `test`, `ci`, `rtsp`, `rtsp-probe`.

## 4. Verification

Verified locally on Windows 11 with Rust 1.99 and VS 2022 Build Tools:
- `cargo build --workspace` (debug and release) succeeds.
- The 36 unit tests pass, including real DPAPI round-trips, `.bak` creation over several
  saves, and the file watcher (external edit, invalid JSON, ignoring the app's own saves).
- `cargo clippy --workspace --all-targets -- -D warnings`, the no-default-features clippy run
  and `cargo fmt --check` are clean.
- Smoke test of `rtspcam-cli`: `config add-test-streams`, `show` and `validate` against a
  scratch config. The password was stored as `dpapi:...`.
- `rtspcam.exe` starts, loads the config, writes a log file and exits.

Not verified yet:
- **CI workflow**: hasn't run on GitHub yet.
- **Test RTSP environment**: `mediamtx` and `ffmpeg` weren't installed on the dev machine, so
  `start.ps1`, `probe.ps1` and `mediamtx.yml` haven't been run. The auth rules and the
  `bluenviron/mediamtx:latest-ffmpeg` image tag in particular should be checked.
- Phase 1 exit criterion "Test RTSP streams are reachable" depends on the item above.

## 5. Decisions and deviations

| Topic | Decision |
|---|---|
| Order of phases | Phase 1 was done before Phase 0. Nothing in Phase 1 depends on the spike results. |
| Unknown `protocol` values | Kept as `Protocol::Unsupported(String)` and saved unchanged, rather than failing the load. |
| Schema version newer than the build | Load is refused with a clear error, to avoid overwriting a newer file. |
| Password that can't be decrypted | Kept as a "locked" secret and flagged by validation, rather than failing the load or being dropped. |
| `on_demand` default | `true` (matches the plan's example config). |
| Log location | `%LOCALAPPDATA%\RtspCam\logs` (local, not roaming); config stays in `%APPDATA%` (roaming). |
| Optional features in core | `watch` and `logging` are default features, turned off for the DLL. |
| Test server port | 8554 instead of 554: no admin needed, and no clash with a real server. |
| License | Not set. Left for the owner to choose. |
| `.rc` / manifest resources, `+crt-static`, installer folder | Deferred to Phases 5 and 7, where the plan uses them. |

## 6. Developer setup notes

- The project needs **Visual Studio Build Tools** with the C++ workload (MSVC linker and
  Windows SDK). The README has the `winget` command.
- **Build from PowerShell, not Git Bash.** Git Bash's `/usr/bin/link` hides MSVC's
  `link.exe`, so linking fails with `link: extra operand`.
- The `x86_64-pc-windows-gnu` toolchain does **not** work as a substitute: `windows-rs` needs
  `dlltool`, which needs an assembler that isn't installed.

## 7. Next steps

1. Push and confirm the CI workflow passes on GitHub.
2. Install `mediamtx` and `ffmpeg` (or use Docker), run `mise run rtsp` and
   `mise run rtsp-probe`, and fix anything in `tools/test-rtsp/`. That closes Phase 1.
3. Start the Phase 0 spikes, beginning with S0.1 (the Rust Media Foundation media source),
   and record the results in the next numbered document.
