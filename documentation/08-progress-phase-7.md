# 08 — Phase 7: packaging and installation

Status: built and compiled; **not yet installed on a clean Windows 11 machine or signed**. The
Phase 7 exit test (clean install → Discord → reboot → uninstall on a fresh VM) needs a person.

## What was added

| Item | Where |
|---|---|
| Installer (Inno Setup 6) | `installer/rtspcam.iss` |
| Build script (release build, optional signing, installer) | `tools/installer/build.ps1` |
| Tag-triggered release workflow | `.github/workflows/release.yml` |
| Version info resources in `rtspcam.exe` and `rtspcam_vcam.dll` | `build.rs` of both crates, `winresource` |
| Static C runtime (no VC++ redistributable) | `.cargo/config.toml` |

Build: `./tools/installer/build.ps1` → `installer\out\RtspCam-<version>-setup.exe` (about 3.8 MB).

## Decisions

| Topic | Decision |
|---|---|
| Installer tool | Inno Setup instead of WiX: one readable script, no extra Rust tooling. The plan allowed either. |
| CLSID registration | The `regserver` flag calls the DLL's own `DllRegisterServer` / `DllUnregisterServer` (HKLM CLSID, log folder with its ACL), so install, repair and uninstall use the code the CLI already uses. Not `regsvr32`. |
| Install location | `C:\Program Files\RtspCam`, 64-bit only (`x64compatible`). |
| Windows 11 check | Installer: `MinVersion=10.0.22000`. App: already refuses with a message when virtual cameras are unsupported. |
| Admin | Required by the installer and uninstaller only. The app is `asInvoker`. |
| Running app | `CloseApplications=yes` (Restart Manager) at install and uninstall, plus a `taskkill` fallback at uninstall. |
| Frame Server | Stopped before files are replaced or removed so the DLL is not locked; Windows restarts it on next use. `restartreplace`/`uninsrestartdelete` cover the case where it still is. |
| `--remove-all-cameras` | Not added. Cameras are session-lifetime (checked in S0.6) and disappear with the process, so closing it is the whole cleanup. |
| Start with Windows | Optional installer task, unchecked. Writes the same HKCU Run value as the app's setting. If an administrator installs for a different user, the entry lands in the administrator's profile; the in-app setting is then the way to enable it. |
| User config | Never deleted by uninstall (`%APPDATA%\RtspCam`). |
| Release profile | Thin LTO was already set. `panic` is left at the default `unwind` (the DLL needs `catch_unwind`). `+crt-static` is set through `.cargo/config.toml`; checked that the exe no longer imports `VCRUNTIME140`. |
| Signing | Optional. `build.ps1 -SignCommand '<signtool ... $f>'` signs the exe and DLL, then the installer and uninstaller. The workflow signs when the secrets `SIGN_PFX_BASE64` and `SIGN_PFX_PASSWORD` exist, otherwise it publishes unsigned. No certificate exists yet. |

## Verified

- Release build of the exe and DLL with version info; `cargo test --workspace`, clippy
  `-D warnings` and `cargo fmt --check` pass.
- The installer script compiles with Inno Setup 6.7.3.

## Not verified (needs a person, ideally on a VM)

1. Running the installer: files in Program Files, camera visible in Discord/OBS/Camera app.
2. Reinstall over a running app and while a camera is in use.
3. Uninstall leaves no files, `HKLM\Software\Classes\CLSID\{...}` key, or Run value; the config is kept.
4. Autostart after reboot.
5. The release workflow on a real tag, and signing with a real certificate (SmartScreen).
6. CI results for the earlier pushes are still unchecked (see 06 §4).

## Next

Phase 8 (GPU path, soak, fuzzing, compatibility matrix, crash reports).
