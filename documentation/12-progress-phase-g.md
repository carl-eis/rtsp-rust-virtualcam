# 12 — Progress: phase G (groundwork for Linux and macOS virtual cameras)

Status (2026-10-10): phase G of [11](11-virtual-cameras-linux-macos-plan.md#3-phase-g--shared-groundwork-portable-small)
is done on `feat/linux-virtual-camera`. Nothing visible changes: no backend uses the new code
yet. Phase L (Linux) followed: [13](13-progress-phase-l.md).

| Step (11 §3) | State |
|---|---|
| G1. Placeholder moved to `rtspcam-ipc` | done |
| G2. `rtspcam_platform::push::Pusher` | done |
| G3. On-demand through `FrameSink::has_reader` | done |
| G4. `Yuyv` and `I420` pixel formats | done |
| G5. Tests | done; run on every OS |

## 1. What was done

### Placeholder (`rtspcam_ipc::placeholder`)

- `crates/rtspcam-vcam/src/placeholder.rs` moved (with `git mv`) to
  `crates/rtspcam-ipc/src/placeholder.rs`; `render` is now public. The 5×7 font and the
  pictures are unchanged.
- Two things the DLL had to itself are now shared, so the DLL and the pusher can't drift
  apart:
  - `STALE_AFTER` (3 s): how long the last frame is shown before the placeholder.
  - `text_for_status(StreamStatus, &str)`: the title and detail for the app's stream status
    ("Connecting...", "No signal", "Camera disabled"). The DLL's `placeholder_text` keeps its
    own cases (camera not set up, app not running, waiting) and calls this for the rest.
- `render` handles the new pixel formats: I420 gives the same bytes as NV12 (gray: Y plane,
  then neutral chroma), YUYV interleaves the same Y values with 128.

### Pusher (`rtspcam_platform::push`)

```rust
pub trait FrameSink: Send + 'static {
    fn write(&mut self, frame: &[u8]) -> io::Result<()>;
    fn has_reader(&mut self) -> Option<bool> { None }
}

Pusher::start(name, format, frames: Arc<dyn FrameSource>, sink: Box<dyn FrameSink>)
    -> io::Result<Pusher>
```

One thread per camera (`rtspcam push <name>`). Each tick, at `format.fps`:

1. Asks the sink for a reader (`None` counts as yes) and calls `client_connected` /
   `client_disconnected` when that changes. With no reader it fetches and writes nothing.
2. Calls `next_frame(format, last_seq, ..)`. A new picture replaces the kept one.
3. Writes the kept picture while it is younger than `STALE_AFTER`, else the placeholder for
   `status()` (rendered again only when its text changes).

Pacing is the DLL's (`deliver()` in `rtspcam-vcam/src/stream.rs`): an absolute deadline that
moves on by one interval per frame, at most one interval of catch-up after a late wake-up, so
there are no bursts. Write errors are logged once (and "works again" once) and delivery goes on.
Dropping the pusher, or `stop()`, stops the thread, waits for it, and calls
`client_disconnected` if a reader was counted.

It has no OS `cfg` and builds on every OS. It sits in `rtspcam-platform` because only camera
backends use it.

### Pixel formats

- `rtspcam_ipc::PixelFormat` has `Yuyv` (code 2, `w*h*2` bytes) and `I420` (code 3,
  `w*h*3/2`). The protocol version stays 1: the DLL never asks for them, and an older DLL would
  only see them if the app sent them, which it doesn't (the client picks the format).
- `rtspcam_pipeline::scale::{nv12_to_yuyv, nv12_to_i420}`. Both copy values (limited range
  stays limited range); YUYV uses each chroma row for the two picture rows it covers.
- The engine's `CameraSource` and the CLI's test source produce all four formats.
- The DLL's `formats::media_type` returns `MF_E_INVALIDMEDIATYPE` for the new two; they are
  never in its advertised list.

## 2. Decisions (beyond the plan)

| Topic | Decision |
|---|---|
| No reader | Nothing is fetched or written, not even the placeholder. Whether a v4l2loopback device needs a writer to keep producing before Chrome lists it (`exclusive_caps=1`) is for L1 to find out; a sink can return `Some(true)` or `None` if it does. |
| New reader | The kept picture is dropped when a reader arrives, so a frame from an earlier session can't flash up. |
| `fps` of 0 | Treated as 1, so a bad spec can't panic the thread (the IPC validation never lets 0 through, but `CameraSpec` comes from config). |
| Shared status text | Pulled into `text_for_status` instead of copying the DLL's match into the pusher. |

## 3. On-demand differs from Windows when the OS can't tell

On Windows, an on-demand stream connects to the RTSP camera only while an app has the camera
open (Frame Server starts and stops the DLL's stream). A push backend gets the same only if
its sink can report readers. If it returns `None`, the pusher counts one reader for as long as
the camera exists, so an on-demand stream runs the whole time its camera is on. Phase L1
checks whether v4l2loopback reports client usage; macOS (M2) plans a device property for it.
Recorded in [10 §5](10-progress-cross-platform.md#5-known-gaps) too.

## 4. Verified

- Windows 11: `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`,
  the DLL-set clippy (`-p rtspcam-core --no-default-features`), `tools/ci/check-os-cfg.sh`,
  and `cargo test --workspace` pass. The 7 pusher tests passed 5 runs in a row.
- Linux (`rust:1-bookworm` in Docker): clippy `-D warnings` and `cargo test --workspace`.
- macOS: not run locally; CI covers it.

New tests:

| Crate | Tests |
|---|---|
| `rtspcam-platform` `push` | pacing (50 fps for 0.5 s gives 15–30 writes), status placeholder and its change, last frame repeated until stale then placeholder only, new frame replaces the placeholder, readers → `client_connected`/`client_disconnected` (none written without a reader, a second reader is a second connection, stop disconnects), `None` reader counts as one until stop, write errors don't stop delivery |
| `rtspcam-ipc` `placeholder` | sizes for all four formats, NV12/I420/YUYV show the same picture, status text matches the DLL's |
| `rtspcam-ipc` `protocol` | `Yuyv` and `I420` round-trip |
| `rtspcam-pipeline` `scale` | YUYV and I420 sample layout |
| `rtspcam-engine` `source` | every pixel format comes out at `frame_len()` |

Not verified (needs a person, same as before this phase): the Windows camera still shows the
same placeholders in an app. The DLL's code path is unchanged apart from where the text and
picture come from, and the moved placeholder tests still pass.

## 5. How to resume

Phase L1 ([11 §4](11-virtual-cameras-linux-macos-plan.md#l1-spike-by-hand-before-writing-the-backend)):
the by-hand v4l2loopback spike. A Linux backend then needs only a `FrameSink` that writes to
`/dev/videoN` and a `VirtualCameraBackend` that starts a `Pusher` per camera.
