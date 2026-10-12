# 22 — Progress: drift and catching up to live, and no lost request tokens (issues #20, #21)

Date: 2026-10-12. Builds [17 §3.1](17-realtimewebcam-comparison.md#31-a-latency-cap-that-catches-up-to-live-medium-measure-first)
(issue #20) and [17 §3.3](17-realtimewebcam-comparison.md#33-never-lose-a-request-token-small-bug-fix)
(issue #21). Both are built and unit-tested; §3 lists what still needs a person.

## 1. Drift and the latency cap (issue #20)

- **Measuring** (`rtspcam-pipeline/src/latency.rs`, `LatencyCap`): each picture's lag is
  computed in the decode thread when it is ready to publish, from its presentation time (the RTP
  timestamp the decoder copies onto the `Frame`) against `Instant::now()`. `now - pts` is when the
  session would have started had this picture come without delay; the earliest value seen is the
  anchor, so the fastest picture has lag 0. Anchoring to the earliest rather than the first
  picture means a late first picture doesn't hide later lag. The lag includes the camera, the
  network, socket buffers and our own `channel(8)` to the decoder, which is the queue that builds
  up when the decoder is slow. The anchor resets on every new session (`DecodeMsg::Start`) and
  after a resync.
- **Shown as** `StreamStats::drift` (average lag over the last second) and in `rtspcam rtsp`
  output. `frames_skipped` and `resyncs` count catch-ups.
- **Latency report** in the stream details (`rtspcam-engine/src/status.rs`, `LatencyReport`):
  arrival to picture and decoder time, drift with its trend (rising, falling or steady, from
  the oldest and newest third of the last 30 s; "measuring" for the first 10 s) and its range,
  and the catch-up counts. It changes at most every 2 s, and it only reads the stats the
  pipeline already sends once a second, so it adds no polling. It starts over when another
  stream is selected or the stream stops.
- **Level 1, skip:** `PipelineOptions::latency_cap` (default 350 ms, `None` only measures).
  While a picture is more than the cap behind, it is decoded but not published to the
  `FrameBus` (saving scaling and conversion in the consumers), until the lag is back within
  half the cap. Skipped pictures still reach the ingest task as `DecodeEvent::Picture` (with
  `shown: false`), so they count as alive for stall detection. No "decode but don't output" mode
  was added to the `Decoder` trait.
- **Level 2, resync:** if skipping lasts more than 1 s (`MAX_CATCH_UP`), or the lag grows by
  another cap's worth while skipping, the decode loop sets `wait_for_key` and sends
  `DecodeEvent::Resync`. Frames until the next key frame are dropped without decoding, which is
  what drains a backlog. Until the next picture, the stall check uses `first_picture_timeout`
  (15 s) from the resync, like the first picture of a session, so a long key-frame interval
  doesn't cause a reconnect.
- **Not done:** no UI setting for the cap; the camera's clock drifting against ours (tens of ppm)
  shows as a very slow rise in drift (0.1 ms/s at 100 ppm), far below the cap.

## 2. MEError instead of a lost request token (issue #21)

`rtspcam-vcam/src/stream.rs`, `send_or_report`: when sending a sample fails with anything but
`MF_E_SHUTDOWN`, the delivery thread retries once with a one-off sample (bypassing the pool). If
that fails too, it queues `MEError` with the HRESULT on the stream's event queue so Frame Server
closes and reopens the stream instead of waiting for a sample that never comes. Delivery goes on
for the other requests. Unit tests drive `send_or_report` with a failing send and a real
`MFCreateEventQueue` queue: retry without the pool, `MEError` with the failure's HRESULT, and no
retry or error on shutdown.

## 3. Still to check

- Drift on real cameras, and on software decoding (OpenH264, Linux and macOS): whether it builds
  up, and whether 350 ms is the right default. The local test streams (mediamtx) weren't running
  for this change.
- How Frame Server and call apps react to `MEError` from the stream in practice. It is rare by
  design (both a pooled and a one-off sample have to fail).
