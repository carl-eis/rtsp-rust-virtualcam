# 07 — Progress: Phase 6 (discovery and convenience)

Status as of 2026-10-09. Continues [06-checkpoint-2026-10-09.md](06-checkpoint-2026-10-09.md)
against [01-plan.md](01-plan.md). The Phase 0 decision record, if it is ever written, is now `08`.

## 1. Summary

| Plan item | Status | Where |
|---|---|---|
| ONVIF WS-Discovery scan ("Find cameras"), credentials, `GetProfiles` / `GetStreamUri` | **Done, checked against real cameras** | `crates/rtspcam-onvif`, `ui/discover_dialog.rs` |
| URL templates for common brands | Done | `rtspcam-core/src/config/templates.rs`, "Camera brand" box in the Add dialog |
| Import / export config JSON (passwords left out) | Done | `rtspcam-core/src/config/transfer.rs`, File menu |
| Per-camera extras: rotate/flip, crop, text overlay, freeze vs. "No signal" | Done | `rtspcam-core/src/config/picture.rs`, `rtspcam-pipeline/src/transform.rs`, `rtspcam-app/src/overlay.rs`, "Picture..." dialog |

Exit criterion ("on a LAN with ONVIF cameras, the user can add a camera without typing its URL")
was exercised on this network: the scan found both Tapo C210 cameras, and with the camera's
login `onvif streams` returned its three profiles with codec, size and RTSP address. The full
click-through (Find cameras -> Get streams -> Add -> OK) has **not** been done by a person; the
dialogs were opened and photographed by script, and the pieces are covered by tests.

## 2. What was added

### ONVIF (`rtspcam-onvif`, new crate)

- **Discovery**: sends a WS-Discovery `Probe` for `NetworkVideoTransmitter` to
  `239.255.255.250:3702` from every non-loopback IPv4 interface (so a PC on two networks
  finds cameras on both), three times over 1.5 s, and collects answers for the wait time
  (4 s in the app). Answers are matched on ONVIF types/scopes: Windows PCs and printers also
  answer WS-Discovery probes and are ignored. The scope `name` and `hardware` give the title.
- **Query** (`query_camera`): reads the camera's clock (`GetSystemDateAndTime`, no login needed)
  so the WS-Security signature uses the camera's time, then `GetDeviceInformation`, the media
  service address (`GetCapabilities`, else `GetServices`, else the device service itself),
  `GetProfiles` and `GetStreamUri` per video profile. Audio-only profiles are skipped. Addresses
  with `0.0.0.0` or `localhost` in them are rewritten to the host the camera was reached on.
- **Errors** are classified (`Unauthorized`, `Network`, `Timeout`, `Fault`, `Protocol`,
  `UnsupportedScheme`) with a hint each, like the pipeline's errors.
- **Small on purpose**: SOAP and HTTP are written by hand (a tokio TCP stream and `quick-xml`),
  so no TLS stack is pulled in. Only plain `http://` device services are supported, which is
  what cameras offer. HTTP-level digest authentication is not implemented; ONVIF's own
  UsernameToken digest is.
- **Tests**: XML tree, the WS-Security digest against the OASIS specification's example, HTTP
  parsing (length and chunked), fault classification, date arithmetic, probe parsing, and a fake
  ONVIF camera on a local socket (login required, clock three hours off, unusable addresses).
  The unauthenticated path was also run against a real camera: `NotAuthorized` is reported as
  "rejected the user name or password".
- **CLI**: `rtspcam-cli onvif discover`, `onvif streams <host[:port]|url> [-u -p |
  --credentials-from <stream name>]`. The last option reads the login saved for a configured
  stream, so the password never appears on a command line.

### Find cameras dialog

File > Find cameras (or the button) scans on opening, lists what answered, and puts the chosen
camera's device address in an editable box. That box also takes a typed `host`, `host:port` or
full URL for cameras that don't answer discovery. Enter the ONVIF login, **Get streams**, choose
a stream, **Add...**: the normal Add dialog opens pre-filled (name from the camera, host, port,
path, login) so Test connection and the rest work as always.

Tapo cameras serve ONVIF on port 2020, and the address box already holds the right one when
chosen from the list. They also need a separate "camera account" (Tapo app > Advanced settings);
the camera's cloud login does not work for either ONVIF or RTSP.

The first scan makes Windows ask whether `rtspcam.exe` may use the network (a UDP socket that
receives answers). Blocking that makes discovery find nothing; typing the address still works.

### Brand templates

The Add dialog has a "Camera brand" box (Hikvision, Dahua, Amcrest, Reolink, TP-Link Tapo,
UniFi Protect, Axis, Foscam; main and sub streams). Choosing one fills **port and path only**.
Opening an existing stream whose port and path match a template selects it. Notes (for example
"turn RTSP on in the Reolink app") appear under the form. The paths are the factory defaults for
channel 1; cameras can be configured differently, so the user can edit both fields.

### Import and export

File > Export streams writes the streams as an ordinary config file (version, streams, no
settings, **no passwords**). File > Import streams adds the streams of an export, or of any
`config.json`:

- every imported stream gets a **new id**, so importing the same file twice makes copies, not a
  clash;
- a name that is taken gets ` (2)`, ` (3)`... appended (compared ignoring case);
- streams that fail validation (bad host, crop that hides everything, ...) are **skipped and
  listed**; the others are still added;
- settings in the file are ignored; an encrypted password from another user or PC is dropped;
- the result is a message box (added / renamed / skipped).

CLI: `rtspcam-cli config export <file>` and `config import <file>`.

### Per-camera picture options

A stream's `picture` object in `config.json` (omitted when everything is default), edited from
the **Picture...** button of the Add/Edit dialog:

```json
"picture": {
  "rotate": "90",                // "0", "90", "180", "270" (clockwise)
  "flip_horizontal": false,
  "flip_vertical": false,
  "crop": { "left": 0, "top": 0, "right": 0, "bottom": 0 },   // percent of each side, 0-80
  "show_name": true,
  "show_time": true,
  "on_disconnect": "no_signal"   // or "freeze_last_frame"
}
```

- **Order**: crop, rotate, flip, then text, then scaling to the output size (so the output size
  and fit mode apply to the adjusted picture; a 90 degree rotation of a 16:9 camera is letterboxed
  in a 16:9 output). Crop edges are rounded down to even pixels because NV12 shares chroma over
  2x2 blocks. `transform::adjust` in the pipeline crate is pure and tested on small pictures:
  both flips, 90/180/270 degrees, four quarter turns being the identity, even-pixel cropping,
  and crop combined with rotation. It is not tested over every combination.
- **Where it runs**: in `CameraShared::latest_frame`, once per decoded picture, cached. The
  preview pane and every app using the camera therefore see the same adjusted picture. With no
  picture options set nothing is copied.
- **Text overlay**: rendered with GDI (Segoe UI, anti-aliased, size relative to the picture's
  height) into a coverage mask, then blended into NV12: a half-dimmed box with white text, bottom
  left. The name is rendered once, the clock once a second. Checked by eye on a test pattern.
- **Freeze vs. No signal**: by default a dropped stream ends in the media source's "No signal"
  picture after a few seconds, as before. With `freeze_last_frame` the app keeps the last
  adjusted picture and re-sends it twice a second while the stream is down (the media source
  would otherwise call it stale after 3 s). It does not apply to "Pause all" or a stream with an
  invalid configuration: those still tell apps "Camera disabled" / the error.

## 3. Bug found on the way: dialogs never initialised

`winsafe`'s `WindowModal` is a window built in code, not a dialog template, so it receives
`WM_CREATE`, **not** `WM_INITDIALOG`. The Add/Edit dialog (Phase 5) registered its start-up code
with `wm_init_dialog`, which therefore never ran: OK was not disabled for an empty form and, more
importantly, the 200 ms timer that shows the **Test connection** result was never started, so
the button could only ever show "Connecting...". Found by opening the new Find dialog (which
copied the pattern) and seeing nothing happen. Both dialogs now use `wm_create`, where the controls
already exist. Test connection was not re-run against a camera in this session; it has the same
code path as before apart from the timer now starting.

## 4. Changes elsewhere

- `StreamConfig.picture` (+ `Field::Picture`, `Problem::InvalidCrop`) in core; the form
  (`StreamForm.picture`, `apply_template`, `stream_from_onvif`, `parse_percent`).
- `rtspcam-app` depends on `winsafe`'s `shell` feature (file dialogs), `rtspcam-onvif`, `url`; the
  `Win32_System_SystemInformation` feature of `windows` (local time for the clock).
- New workspace dependencies in `rtspcam-onvif`: `quick-xml`, `sha1`, `socket2`, `if-addrs`.
- The main window has a **Find cameras** button; File has Find cameras, Import, Export.

## 5. Not verified, needs a person

1. The whole Find cameras flow by hand, including adding a camera and seeing it in Discord.
2. A camera whose ONVIF needs HTTP digest authentication, or only HTTPS (both reported as errors,
   not handled).
3. Discovery on a PC with several adapters or a VPN (each IPv4 interface is probed; only
   one network was available).
4. The Picture options and overlay on a live camera in a call; only pictures from a test pattern
   were looked at, and the unit tests.
5. Test connection against a camera after the timer fix (see section 3).
6. Import/export through the Windows file dialogs (written against winsafe's documented example,
   compiled, but the dialogs were not opened).

## 6. Next

Phase 7 (installer, signing, Windows 11 check) is still the blocker for anyone but the developer;
Phase 8 (GPU path, soak, fuzzing, crash reports) follows. The person-only checks in
[06](06-checkpoint-2026-10-09.md) section 4 and section 5 above come before either.
