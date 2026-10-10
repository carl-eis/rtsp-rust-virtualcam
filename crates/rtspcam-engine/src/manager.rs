//! The camera manager: one pipeline and one virtual camera per enabled stream, kept in step with
//! the configuration.
//!
//! ```text
//!  Config ──apply──► reconciler task ──► per camera:
//!                                          ├─ supervisor    (starts/stops the RTSP pipeline)
//!                                          └─ virtual camera (via the VirtualCameraBackend,
//!                                                             which gets the camera's frames
//!                                                             and counts its consumers)
//! ```
//!
//! [`CameraManager::apply`] never blocks: it hands the new configuration to a task that works
//! out what changed and does the slow parts (creating cameras can take a second) in order.
//! Streams that changed are rebuilt from scratch, except that their virtual camera is re-created
//! in place rather than removed and added (an app using it keeps it); the rest are left alone.
//!
//! On-demand streams connect only while an app uses the webcam or a [`Preview`] is open, and
//! disconnect [`idle_grace`](ManagerOptions::idle_grace) after the last one leaves.
//!
//! A "Source" stream's camera offers the camera's own picture size, which is known only once
//! the stream connects. The camera is created with the size seen last time (remembered in
//! [`ManagerOptions::source_sizes`]), or the configured size before the first connection. When
//! the stream turns out to have another size, the virtual camera is re-created with it as soon
//! as no app is using it; until then apps get the pictures scaled to the size they chose.

use std::collections::HashMap;
use std::io;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError, RwLock};
use std::thread;
use std::time::Duration;

use rtspcam_core::config::{OnDisconnect, Picture, Rotation, StreamConfig};
use rtspcam_core::profile::{self, Stage};
use rtspcam_core::{Config, FitMode, OutputFormat};
use rtspcam_ipc::source::PictureSignal;
use rtspcam_pipeline::transform;
use rtspcam_pipeline::{
    AtomicSize, Frame, FrameBus, Pipeline, PipelineError, PipelineOptions, SourceOptions,
    StreamState,
};
use rtspcam_platform::{CameraError, CameraSpec, VirtualCameraBackend};
use tokio::runtime::{Handle, Runtime};
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;
use tokio::time::Instant;
use uuid::Uuid;

use crate::overlay::{Overlay, local_time_text};
use crate::source::CameraSource;
pub use crate::source_sizes::FILE_NAME as SOURCE_SIZES_FILE;
use crate::source_sizes::SourceSizes;
use crate::status::{Activity, CameraStatus, VcamState};

type Notify = Arc<dyn Fn() + Send + Sync>;

/// How the manager behaves.
#[derive(Clone)]
pub struct ManagerOptions {
    /// An on-demand stream stays connected this long after its last consumer leaves, so
    /// switching cameras in a call doesn't tear the connection down and up again.
    pub idle_grace: Duration,
    /// Called (from any thread) whenever something the UI shows may have changed.
    pub on_change: Notify,
    /// Tell each pipeline the largest picture its consumers and preview ask for, so a
    /// decoder that can shrink pictures cheaply (on the GPU) does it right away.
    pub limit_decoded_size: bool,
    /// Where to remember the picture size of each "Source" stream's camera across runs
    /// (normally [`SOURCE_SIZES_FILE`] in the config folder). `None`: in memory only.
    pub source_sizes: Option<PathBuf>,
}

impl Default for ManagerOptions {
    fn default() -> Self {
        Self {
            idle_grace: Duration::from_secs(10),
            on_change: Arc::new(|| {}),
            limit_decoded_size: true,
            source_sizes: None,
        }
    }
}

impl std::fmt::Debug for ManagerOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ManagerOptions")
            .field("idle_grace", &self.idle_grace)
            .field("limit_decoded_size", &self.limit_decoded_size)
            .field("source_sizes", &self.source_sizes)
            .finish_non_exhaustive()
    }
}

/// How many things want a camera's pictures.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Demand {
    clients: usize,
    previews: usize,
}

/// A size asked for is forgotten when nobody asked for it this long (its consumer left).
const REQUEST_TTL: Duration = Duration::from_secs(3);
/// A size asked for again within this long changes nothing, so the limit isn't worked out
/// again for every frame a consumer reads.
const REQUEST_REFRESH: Duration = Duration::from_millis(500);

/// The picture sizes consumers and the preview asked for recently, and the decoder size limit
/// worked out from them.
#[derive(Debug, Default)]
struct Requests {
    /// Size and how the picture is fitted into it.
    seen: HashMap<(u32, u32, FitMode), std::time::Instant>,
    limit: Option<(u32, u32)>,
    /// The camera's picture size `limit` was worked out with.
    source: Option<(u32, u32)>,
}

/// The decoded picture size needed to show `width` x `height` fitted with `fit`, after the
/// picture options, as a box for [`Decoder::limit_size`](rtspcam_pipeline::decode::Decoder::limit_size):
/// rotation swaps the sides, and cropping shows only part of the picture, so the whole picture
/// must be larger by the part cut away. A letterboxed picture needs only the scale that fits
/// it inside, which depends on the camera's picture shape (`source`, once known).
fn source_size_for(
    width: u32,
    height: u32,
    fit: FitMode,
    picture: &Picture,
    source: Option<(u32, u32)>,
) -> (u32, u32) {
    let (w, h) = match picture.rotate {
        Rotation::Cw90 | Rotation::Cw270 => (height, width),
        Rotation::None | Rotation::Cw180 => (width, height),
    };
    let c = picture.crop;
    let keep = |a: u8, b: u8| 100u32.saturating_sub(u32::from(a) + u32::from(b)).max(1);
    let (keep_w, keep_h) = (keep(c.left, c.right), keep(c.top, c.bottom));
    match (fit, source) {
        (FitMode::Letterbox, Some((sw, sh))) if sw > 0 && sh > 0 => {
            // The part shown (sw * keep_w / 100 by sh * keep_h / 100) is scaled to fit inside
            // w x h; the whole picture at that scale is the box. Integer maths, so exact
            // ratios stay exact.
            let (w, h, sw, sh) = (u64::from(w), u64::from(h), u64::from(sw), u64::from(sh));
            let (kw, kh) = (u64::from(keep_w), u64::from(keep_h));
            let (bw, bh) = if w * sh * kh <= h * sw * kw {
                // Width decides: scale = w * 100 / (sw * kw).
                ((w * 100).div_ceil(kw), (sh * w * 100).div_ceil(sw * kw))
            } else {
                ((sw * h * 100).div_ceil(sh * kh), (h * 100).div_ceil(kh))
            };
            (bw as u32, bh as u32)
        }
        _ => ((w * 100).div_ceil(keep_w), (h * 100).div_ceil(keep_h)),
    }
}

/// Remembers the adjusted picture so every reader gets the same one without redoing the work.
#[derive(Default)]
struct Adjusted {
    /// The decoded picture `result` was made from, and the clock text drawn on it.
    from: Option<(Arc<Frame>, String)>,
    result: Option<Arc<Frame>>,
    overlay: Overlay,
    /// The last picture handed out (kept for "freeze last frame").
    shown: Option<Arc<Frame>>,
}

impl Adjusted {
    fn adjust(&mut self, raw: Arc<Frame>, name: &str, picture: &Picture) -> Arc<Frame> {
        if !picture.changes_frames() {
            return raw;
        }
        let clock = if picture.show_time {
            local_time_text()
        } else {
            String::new()
        };
        if let (Some((from, at)), Some(result)) = (&self.from, &self.result)
            && Arc::ptr_eq(from, &raw)
            && *at == clock
        {
            return result.clone();
        }
        let _t = profile::time(Stage::Adjust);
        let mut frame = match transform::adjust(&raw, picture) {
            Some(f) => f,
            None => (*raw).clone(),
        };
        let mut lines = Vec::new();
        if picture.show_name {
            lines.push(name.to_owned());
        }
        if picture.show_time {
            lines.push(clock.clone());
        }
        if !lines.is_empty() {
            frame = self.overlay.draw(frame, &lines);
        }
        let result = Arc::new(frame);
        self.from = Some((raw, clock));
        self.result = Some(result.clone());
        result
    }
}

/// State shared by a camera's pipe server, supervisor, previews and the manager.
pub(crate) struct CameraShared {
    pub(crate) id: Uuid,
    pub(crate) name: String,
    pub(crate) fit: FitMode,
    picture: Picture,
    /// The adjusted copy of the newest picture, and the last one shown (for "freeze").
    adjusted: Mutex<Adjusted>,
    bus: RwLock<Option<FrameBus>>,
    activity: watch::Sender<Activity>,
    demand: watch::Sender<Demand>,
    vcam: Mutex<VcamState>,
    pipeline: Mutex<Option<Pipeline>>,
    /// Sizes asked for, for the decoder's size limit (`None`: don't limit).
    requests: Option<Mutex<Requests>>,
    /// The camera's picture size, once a picture arrived (`Frame::source_size`).
    source_size: AtomicSize,
    /// What the virtual camera lists first: [`CameraSpec::preferred`].
    offered: Mutex<(u32, u32, u32)>,
    notify: Notify,
}

impl CameraShared {
    pub(crate) fn new(config: &StreamConfig, notify: Notify) -> Self {
        Self {
            id: config.id,
            name: config.name.clone(),
            fit: config.fit_mode,
            picture: config.picture,
            adjusted: Mutex::default(),
            bus: RwLock::new(None),
            activity: watch::Sender::new(Activity::Idle),
            demand: watch::Sender::new(Demand::default()),
            vcam: Mutex::new(VcamState::Pending),
            pipeline: Mutex::new(None),
            requests: Some(Mutex::default()),
            source_size: AtomicSize::default(),
            offered: Mutex::new((config.output.width, config.output.height, config.output.fps)),
            notify,
        }
    }

    /// Never limits the decoded size.
    fn without_size_limit(mut self) -> Self {
        self.requests = None;
        self
    }

    /// Someone (a consumer, the preview) asked for a `width` x `height` picture. Keeps the
    /// pipeline's size limit at the largest size anyone asked for in the last few seconds.
    pub(crate) fn note_request(&self, width: u32, height: u32, fit: FitMode) {
        let Some(requests) = &self.requests else {
            return;
        };
        let now = std::time::Instant::now();
        let source = self.source_size.get();
        let key = (width, height, fit);
        let mut r = requests.lock().unwrap_or_else(PoisonError::into_inner);
        // Most calls repeat a size asked for moments ago, for the same camera picture.
        if r.source == source
            && r.seen
                .get(&key)
                .is_some_and(|t| now.duration_since(*t) < REQUEST_REFRESH)
        {
            return;
        }
        // A handful of entries at most (one per distinct size), so recomputing is cheap.
        r.seen.insert(key, now);
        r.seen.retain(|_, t| now.duration_since(*t) < REQUEST_TTL);
        r.source = source;
        let limit = r
            .seen
            .keys()
            .map(|&(w, h, fit)| source_size_for(w, h, fit, &self.picture, source))
            .reduce(|a, b| (a.0.max(b.0), a.1.max(b.1)));
        if limit != r.limit {
            tracing::debug!(camera = %self.name, ?limit, "decoded size limit");
            r.limit = limit;
            if let Some(p) = self
                .pipeline
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .as_ref()
            {
                p.set_size_limit(limit);
            }
        }
    }

    /// Finishes when the stream publishes a picture (see [`FrameSource::picture_signal`]).
    ///
    /// [`FrameSource::picture_signal`]: rtspcam_ipc::FrameSource::picture_signal
    pub(crate) fn picture_signal(&self) -> Option<PictureSignal> {
        let mut rx = self
            .bus
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()?
            .subscribe();
        Some(Box::pin(async move {
            let _ = rx.changed().await;
        }))
    }

    /// The current decoded size limit.
    #[cfg(test)]
    fn size_limit(&self) -> Option<(u32, u32)> {
        self.requests
            .as_ref()?
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .limit
    }

    /// Makes `pipeline` the camera's, with the current decoded size limit. Holds the requests
    /// lock throughout (the same order as [`note_request`](Self::note_request)), so a limit
    /// worked out meanwhile isn't lost.
    fn install_pipeline(&self, pipeline: Pipeline) {
        let requests = self
            .requests
            .as_ref()
            .map(|r| r.lock().unwrap_or_else(PoisonError::into_inner));
        pipeline.set_size_limit(requests.as_ref().and_then(|r| r.limit));
        *self.pipeline.lock().unwrap_or_else(PoisonError::into_inner) = Some(pipeline);
    }

    /// The newest picture with the stream's picture settings applied (crop, rotate, flip, text).
    ///
    /// With "freeze last frame" the last picture is kept while the stream is down, so apps keep
    /// seeing it instead of "No signal".
    pub(crate) fn latest_frame(&self) -> Option<Arc<Frame>> {
        let raw = self
            .bus
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
            .and_then(FrameBus::latest);
        let mut adjusted = self.adjusted.lock().unwrap_or_else(PoisonError::into_inner);
        let Some(raw) = raw else {
            return self
                .holds_last_frame()
                .then(|| adjusted.shown.clone())
                .flatten();
        };
        self.source_size.set(Some(raw.source_size));
        let shown = adjusted.adjust(raw, &self.name, &self.picture);
        if self.picture.on_disconnect == OnDisconnect::FreezeLastFrame {
            adjusted.shown = Some(shown.clone());
        }
        Some(shown)
    }

    /// Whether apps should keep seeing the last picture now: asked for, and the camera is not
    /// switched off or misconfigured.
    pub(crate) fn holds_last_frame(&self) -> bool {
        self.picture.on_disconnect == OnDisconnect::FreezeLastFrame
            && !matches!(self.activity(), Activity::Paused | Activity::Invalid(_))
    }

    pub(crate) fn activity(&self) -> Activity {
        self.activity.borrow().clone()
    }

    pub(crate) fn add_clients(&self, delta: isize) {
        self.demand
            .send_modify(|d| d.clients = d.clients.saturating_add_signed(delta));
        (self.notify)();
    }

    fn add_previews(&self, delta: isize) {
        self.demand
            .send_modify(|d| d.previews = d.previews.saturating_add_signed(delta));
        (self.notify)();
    }

    pub(crate) fn set_bus(&self, bus: Option<FrameBus>) {
        *self.bus.write().unwrap_or_else(PoisonError::into_inner) = bus;
    }

    fn set_activity(&self, activity: Activity) {
        let changed = self.activity.send_if_modified(|a| {
            let changed = *a != activity;
            if changed {
                *a = activity;
            }
            changed
        });
        if changed {
            (self.notify)();
        }
    }

    /// Records the outcome of creating the camera, unless something already went wrong.
    fn vcam_created(&self, result: Result<(), CameraError>) {
        {
            let mut vcam = self.vcam.lock().unwrap_or_else(PoisonError::into_inner);
            if *vcam == VcamState::Pending {
                *vcam = match result {
                    Ok(()) => VcamState::Ready,
                    Err(CameraError::Unsupported(why)) => VcamState::Unsupported(why),
                    Err(CameraError::Failed(why)) => VcamState::Failed(why),
                };
            }
        }
        (self.notify)();
    }

    fn status(&self) -> CameraStatus {
        let demand = *self.demand.borrow();
        CameraStatus {
            id: self.id,
            name: self.name.clone(),
            activity: self.activity(),
            vcam: self
                .vcam
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone(),
            clients: demand.clients,
            previews: demand.previews,
            offered: *self.offered.lock().unwrap_or_else(PoisonError::into_inner),
        }
    }

    /// Takes the running pipeline (if any) so it can be stopped.
    fn take_pipeline(&self) -> Option<Pipeline> {
        self.pipeline
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
    }
}

/// Keeps a stream's pictures flowing while it is being looked at, for example in the preview
/// pane. On-demand streams connect while one exists. Dropping it releases the stream.
pub struct Preview {
    camera: Arc<CameraShared>,
}

impl std::fmt::Debug for Preview {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Preview")
            .field("camera", &self.camera.name)
            .finish()
    }
}

impl Preview {
    /// The newest decoded picture, at the source's resolution.
    pub fn latest(&self) -> Option<Arc<Frame>> {
        self.camera.latest_frame()
    }

    /// [`latest`](Self::latest), for showing it within `width` x `height` pixels. The size
    /// counts toward how large the decoder's pictures need to be.
    pub fn latest_for(&self, width: u32, height: u32) -> Option<Arc<Frame>> {
        if width >= 2 && height >= 2 {
            self.camera.note_request(width, height, FitMode::Letterbox);
        }
        self.camera.latest_frame()
    }

    pub fn activity(&self) -> Activity {
        self.camera.activity()
    }
}

impl Drop for Preview {
    fn drop(&mut self) {
        self.camera.add_previews(-1);
    }
}

struct Camera {
    config: StreamConfig,
    state: Arc<CameraShared>,
    supervisor: JoinHandle<()>,
    /// For a "Source" stream: watches the camera's picture size.
    follower: Option<JoinHandle<()>>,
}

impl Camera {
    fn abort_tasks(&self) {
        self.supervisor.abort();
        if let Some(f) = &self.follower {
            f.abort();
        }
    }
}

/// A "Source" stream's camera sends `size` pictures: remember the size, and with `recreate`
/// re-create the virtual camera with it.
struct Resize {
    camera: Arc<CameraShared>,
    size: (u32, u32),
    recreate: bool,
}

struct Shared {
    cameras: Mutex<HashMap<Uuid, Camera>>,
    paused: watch::Sender<bool>,
    backend: Arc<dyn VirtualCameraBackend>,
    options: ManagerOptions,
    handle: Handle,
    source_sizes: SourceSizes,
    /// To the reconciler, which also creates and removes the cameras.
    resize: mpsc::UnboundedSender<Resize>,
}

/// Owns every camera. See the [module docs](self).
pub struct CameraManager {
    runtime: Option<Runtime>,
    shared: Arc<Shared>,
    config_tx: watch::Sender<Arc<Config>>,
    reconciler: JoinHandle<()>,
}

impl std::fmt::Debug for CameraManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CameraManager").finish_non_exhaustive()
    }
}

impl CameraManager {
    /// Starts the manager with no cameras; call [`apply`](Self::apply) to give it the config.
    pub fn start(
        backend: Arc<dyn VirtualCameraBackend>,
        options: ManagerOptions,
    ) -> io::Result<Self> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .thread_name("rtspcam")
            .build()?;
        let (resize, resize_rx) = mpsc::unbounded_channel();
        let shared = Arc::new(Shared {
            cameras: Mutex::default(),
            paused: watch::Sender::new(false),
            backend,
            source_sizes: SourceSizes::load(options.source_sizes.clone()),
            options,
            handle: runtime.handle().clone(),
            resize,
        });
        let (config_tx, config_rx) = watch::channel(Arc::new(Config::default()));
        let reconciler = runtime.spawn(reconcile_loop(shared.clone(), config_rx, resize_rx));
        Ok(Self {
            runtime: Some(runtime),
            shared,
            config_tx,
            reconciler,
        })
    }

    /// A handle to the manager's tokio runtime.
    pub fn handle(&self) -> &Handle {
        &self.shared.handle
    }

    /// Makes the cameras match `config`. Returns immediately; the work happens in the
    /// background, and [`ManagerOptions::on_change`] fires as cameras come and go.
    ///
    /// The "Use hardware decoding" setting takes effect here too: before any stream of this
    /// config starts, and for running streams at their next key frame (without reconnecting).
    pub fn apply(&self, config: &Config) {
        rtspcam_pipeline::decode::set_use_hardware(config.settings.hardware_decoding);
        self.config_tx.send_replace(Arc::new(config.clone()));
    }

    /// Closes every RTSP connection and shows "Camera disabled" in apps, or undoes that.
    pub fn set_paused(&self, paused: bool) {
        self.shared.paused.send_replace(paused);
        (self.shared.options.on_change)();
    }

    pub fn is_paused(&self) -> bool {
        *self.shared.paused.borrow()
    }

    /// The state of every camera, in no particular order.
    pub fn statuses(&self) -> Vec<CameraStatus> {
        self.cameras().iter().map(|c| c.status()).collect()
    }

    pub fn status(&self, id: Uuid) -> Option<CameraStatus> {
        self.camera(id).map(|c| c.status())
    }

    /// Opens a preview of stream `id` (`None` if it has no camera, for example it is disabled).
    pub fn open_preview(&self, id: Uuid) -> Option<Preview> {
        let camera = self.camera(id)?;
        camera.add_previews(1);
        Some(Preview { camera })
    }

    fn cameras(&self) -> Vec<Arc<CameraShared>> {
        self.shared
            .cameras
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .values()
            .map(|c| c.state.clone())
            .collect()
    }

    fn camera(&self, id: Uuid) -> Option<Arc<CameraShared>> {
        self.shared
            .cameras
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&id)
            .map(|c| c.state.clone())
    }

    /// Shuts everything down in order and waits for it:
    ///
    /// 1. stop configuration changes,
    /// 2. stop frame delivery (no new consumers) and remove every virtual camera (they
    ///    disappear from Discord and the like),
    /// 3. stop the RTSP pipelines and join their decode threads,
    /// 4. stop the async runtime.
    ///
    /// If this takes longer than `limit` the process is ended anyway: quitting must always
    /// end the process.
    pub fn shutdown(mut self, limit: Duration) {
        let started = std::time::Instant::now();
        let watchdog = thread::Builder::new()
            .name("shutdown watchdog".into())
            .spawn(move || {
                thread::sleep(limit);
                tracing::warn!(?limit, "shutdown is stuck, ending the process");
                std::process::exit(0);
            });
        if let Err(e) = watchdog {
            tracing::warn!(error = %e, "could not start the shutdown watchdog");
        }
        self.shutdown_in_order();
        tracing::info!(elapsed = ?started.elapsed(), "shutdown complete");
    }

    fn shutdown_in_order(&mut self) {
        self.reconciler.abort();
        let cameras: Vec<Camera> = self
            .shared
            .cameras
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .drain()
            .map(|(_, c)| c)
            .collect();
        for c in &cameras {
            c.abort_tasks();
        }
        self.shared.backend.remove_all();
        for c in &cameras {
            c.state.set_bus(None);
            if let Some(p) = c.state.take_pipeline() {
                p.stop();
            }
        }
        if let Some(rt) = self.runtime.take() {
            rt.shutdown_timeout(Duration::from_secs(2));
        }
    }
}

impl Drop for CameraManager {
    fn drop(&mut self) {
        if self.runtime.is_some() {
            self.shutdown_in_order();
        }
    }
}

/// Applies configs and re-creates "Source" cameras whose size changed, one thing at a time, so
/// a camera is never created and removed at once.
async fn reconcile_loop(
    shared: Arc<Shared>,
    mut rx: watch::Receiver<Arc<Config>>,
    mut resize: mpsc::UnboundedReceiver<Resize>,
) {
    loop {
        tokio::select! {
            r = rx.changed() => {
                if r.is_err() {
                    return;
                }
                let config = rx.borrow_and_update().clone();
                reconcile(&shared, &config).await;
            }
            Some(r) = resize.recv() => resize_camera(&shared, r).await,
        }
    }
}

async fn reconcile(shared: &Arc<Shared>, config: &Config) {
    shared
        .source_sizes
        .retain(|id| config.stream(id).is_some_and(|s| s.output.source));
    let wanted: Vec<&StreamConfig> = config.streams.iter().filter(|s| s.enabled).collect();

    // Remove what is gone or changed.
    let stale: Vec<Camera> = {
        let mut cameras = shared
            .cameras
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let ids: Vec<Uuid> = cameras
            .iter()
            .filter(|(id, cam)| !wanted.iter().any(|s| s.id == **id && **s == cam.config))
            .map(|(id, _)| *id)
            .collect();
        ids.iter().filter_map(|id| cameras.remove(id)).collect()
    };
    for camera in stale {
        // An edited stream keeps its virtual camera: the backend re-creates it in place when
        // the stream is added again below, so an app using it (a call) stays on the same
        // device. Removing it first would let Linux move it to another device.
        let edited = wanted.iter().any(|s| s.id == camera.state.id);
        if edited {
            tracing::info!(id = %camera.state.id, name = %camera.state.name, "rebuilding camera");
        } else {
            tracing::info!(id = %camera.state.id, name = %camera.state.name, "removing camera");
        }
        remove_camera(shared, camera, !edited).await;
    }

    // Add what is new.
    for stream in wanted {
        let exists = shared
            .cameras
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .contains_key(&stream.id);
        if !exists {
            add_camera(shared, stream).await;
        }
    }
    (shared.options.on_change)();
}

/// Stops a camera's pipeline, and removes its virtual camera when `remove_vcam`.
async fn remove_camera(shared: &Arc<Shared>, camera: Camera, remove_vcam: bool) {
    camera.abort_tasks();
    camera.state.set_bus(None);
    if let Some(pipeline) = camera.state.take_pipeline() {
        let _ = tokio::task::spawn_blocking(move || pipeline.stop()).await;
    }
    if remove_vcam {
        let backend = shared.backend.clone();
        let id = camera.state.id;
        let _ = tokio::task::spawn_blocking(move || backend.remove(id)).await;
    }
    (shared.options.on_change)();
}

async fn add_camera(shared: &Arc<Shared>, config: &StreamConfig) {
    tracing::info!(id = %config.id, name = %config.name, url = %config.url(), "adding camera");
    let mut state = CameraShared::new(config, shared.options.on_change.clone());
    if !shared.options.limit_decoded_size {
        state = state.without_size_limit();
    }
    let preferred = offered_output(config, shared.source_sizes.get(config.id));
    *state.offered.lock().unwrap_or_else(PoisonError::into_inner) = preferred;
    let state = Arc::new(state);

    let supervisor = shared.handle.spawn(supervise(
        state.clone(),
        SourceOptions::from_stream(config),
        config.on_demand,
        shared.paused.subscribe(),
        shared.options.idle_grace,
    ));
    let follower = config.output.source.then(|| {
        shared.handle.spawn(follow_source_size(
            state.clone(),
            shared.resize.clone(),
            (preferred.0, preferred.1),
        ))
    });

    shared
        .cameras
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .insert(
            config.id,
            Camera {
                config: config.clone(),
                state: state.clone(),
                supervisor,
                follower,
            },
        );
    (shared.options.on_change)();

    let result = create_vcam(shared, &state, preferred).await;
    state.vcam_created(result);
}

/// What a stream's camera lists first: the configured output, or for a "Source" stream the
/// camera's picture size (`seen`, if it has connected before).
fn offered_output(config: &StreamConfig, seen: Option<(u32, u32)>) -> (u32, u32, u32) {
    let o = config.output;
    let (width, height) = if o.source {
        seen.or_else(|| OutputFormat::source_size(o.width, o.height))
            .unwrap_or(OutputFormat::SOURCE_FALLBACK)
    } else {
        (o.width, o.height)
    };
    (width, height, o.fps)
}

/// Creates (or re-creates) `camera`'s virtual camera, offering `preferred` first. Creating the
/// camera is the slow part; the backend gets the camera's frames with it.
async fn create_vcam(
    shared: &Arc<Shared>,
    camera: &Arc<CameraShared>,
    preferred: (u32, u32, u32),
) -> Result<(), CameraError> {
    let backend = shared.backend.clone();
    let frames = Arc::new(CameraSource::new(camera.clone()));
    let spec = CameraSpec {
        id: camera.id,
        name: camera.name.clone(),
        preferred,
    };
    let id = spec.id;
    let runtime = shared.handle.clone();
    let result = tokio::task::spawn_blocking(move || backend.create(&spec, frames, &runtime))
        .await
        .unwrap_or_else(|e| Err(CameraError::Failed(e.to_string())));
    match &result {
        Ok(()) => {}
        Err(CameraError::Unsupported(why)) => tracing::info!(%id, "{why}"),
        Err(CameraError::Failed(why)) => {
            tracing::error!(%id, error = %why, "could not create the virtual camera");
        }
    }
    result
}

/// Watches a "Source" stream's picture size. When it differs from the size the virtual camera
/// offers (`offered`), remembers it and asks for the camera to be re-created with it once no
/// app is using the camera: re-creating it under an app (a call) would take the camera away
/// from it. Meanwhile apps get pictures scaled to the size they chose.
async fn follow_source_size(
    camera: Arc<CameraShared>,
    resize: mpsc::UnboundedSender<Resize>,
    mut offered: (u32, u32),
) {
    let mut activity = camera.activity.subscribe();
    let mut demand = camera.demand.subscribe();
    let mut seen = None;
    loop {
        let send = |size, recreate| {
            resize
                .send(Resize {
                    camera: camera.clone(),
                    size,
                    recreate,
                })
                .is_ok()
        };
        if let Activity::Running(StreamState::Streaming(stats)) = &*activity.borrow_and_update()
            && let Some(size) = OutputFormat::source_size(stats.width, stats.height)
            && seen != Some(size)
        {
            seen = Some(size);
            // Remembered now, so the next run offers it even if an app keeps the camera.
            if !send(size, false) {
                return;
            }
        }
        let in_use = demand.borrow_and_update().clients > 0;
        if let Some(size) = seen
            && size != offered
            && !in_use
        {
            tracing::info!(
                camera = %camera.name,
                from = ?offered,
                to = ?size,
                "the camera's picture size differs from what its virtual camera offers"
            );
            if !send(size, true) {
                return;
            }
            offered = size;
        }
        tokio::select! {
            r = activity.changed() => if r.is_err() { return },
            r = demand.changed() => if r.is_err() { return },
        }
    }
}

/// Re-creates a "Source" stream's virtual camera with the camera's picture size, and remembers
/// the size for next time. Nothing happens if the stream was removed or edited meanwhile (its
/// camera was rebuilt), or its virtual camera doesn't exist.
async fn resize_camera(
    shared: &Arc<Shared>,
    Resize {
        camera,
        size,
        recreate,
    }: Resize,
) {
    let current = shared
        .cameras
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .get(&camera.id)
        .is_some_and(|c| Arc::ptr_eq(&c.state, &camera));
    if !current {
        return;
    }
    shared.source_sizes.set(camera.id, size);
    if !recreate {
        return;
    }
    let ready = *camera.vcam.lock().unwrap_or_else(PoisonError::into_inner) == VcamState::Ready;
    if !ready {
        return;
    }
    let preferred = {
        let mut offered = camera
            .offered
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        *offered = (size.0, size.1, offered.2);
        *offered
    };
    tracing::info!(camera = %camera.name, ?preferred, "re-creating the camera with its picture size");
    if let Err(e) = create_vcam(shared, &camera, preferred).await {
        *camera.vcam.lock().unwrap_or_else(PoisonError::into_inner) = match e {
            CameraError::Unsupported(why) => VcamState::Unsupported(why),
            CameraError::Failed(why) => VcamState::Failed(why),
        };
    }
    (shared.options.on_change)();
}

/// Starts and stops a camera's RTSP pipeline as demand and the pause switch change.
async fn supervise(
    camera: Arc<CameraShared>,
    source: Result<SourceOptions, PipelineError>,
    on_demand: bool,
    mut paused: watch::Receiver<bool>,
    idle_grace: Duration,
) {
    let source = match source {
        Ok(s) => s,
        Err(e) => {
            camera.set_activity(Activity::Invalid(e.to_string()));
            return;
        }
    };
    let handle = Handle::current();
    let mut demand = camera.demand.subscribe();
    let mut status: Option<watch::Receiver<StreamState>> = None;
    let mut idle_since: Option<Instant> = None;

    loop {
        let is_paused = *paused.borrow_and_update();
        let d = *demand.borrow_and_update();
        let wanted = !is_paused && (!on_demand || d.clients + d.previews > 0);

        if wanted {
            idle_since = None;
            if status.is_none() {
                let pipeline = Pipeline::start(
                    camera.name.clone(),
                    PipelineOptions::new(source.clone()),
                    &handle,
                );
                camera.set_bus(Some(pipeline.frames().clone()));
                let rx = pipeline.status();
                camera.set_activity(Activity::Running(rx.borrow().clone()));
                camera.install_pipeline(pipeline);
                status = Some(rx);
            }
        } else if status.is_some() {
            // Pausing disconnects at once; running out of consumers waits out the grace.
            let since = *idle_since.get_or_insert_with(Instant::now);
            if is_paused || since.elapsed() >= idle_grace {
                idle_since = None;
                status = None;
                camera.set_bus(None);
                if let Some(pipeline) = camera.take_pipeline() {
                    tokio::task::spawn_blocking(move || pipeline.stop());
                }
            }
        }
        if status.is_none() {
            camera.set_activity(if is_paused {
                Activity::Paused
            } else {
                Activity::Idle
            });
        }

        let deadline = idle_since.map(|s| s + idle_grace);
        let pipeline_changed = async {
            if let Some(rx) = status.as_mut()
                && rx.changed().await.is_ok()
            {
                return rx.borrow_and_update().clone();
            }
            std::future::pending().await
        };
        let grace_over = async {
            match deadline {
                Some(at) => tokio::time::sleep_until(at).await,
                None => std::future::pending().await,
            }
        };
        tokio::select! {
            r = paused.changed() => if r.is_err() { return },
            r = demand.changed() => if r.is_err() { return },
            state = pipeline_changed => camera.set_activity(Activity::Running(state)),
            () = grace_over => {}
        }
    }
}

#[cfg(test)]
mod tests {

    use super::*;

    fn camera(picture: Picture) -> CameraShared {
        let mut config = StreamConfig::new("Door", "10.0.0.2");
        config.picture = picture;
        CameraShared::new(&config, Arc::new(|| {}))
    }

    fn publish(camera: &CameraShared, frame: Frame) -> FrameBus {
        let bus = FrameBus::new();
        bus.publish(frame);
        camera.set_bus(Some(bus.clone()));
        bus
    }

    #[test]
    fn unchanged_pictures_pass_through() {
        let cam = camera(Picture::default());
        let bus = publish(&cam, Frame::black(64, 32));
        assert!(Arc::ptr_eq(
            &cam.latest_frame().unwrap(),
            &bus.latest().unwrap()
        ));
    }

    #[test]
    fn rotation_is_applied_once_per_picture() {
        let cam = camera(Picture {
            rotate: Rotation::Cw90,
            ..Picture::default()
        });
        let bus = publish(&cam, Frame::black(64, 32));
        let first = cam.latest_frame().unwrap();
        assert_eq!((first.width(), first.height()), (32, 64));
        assert!(
            Arc::ptr_eq(&first, &cam.latest_frame().unwrap()),
            "the same picture is not processed twice"
        );
        bus.publish(Frame::black(64, 32));
        assert!(!Arc::ptr_eq(&first, &cam.latest_frame().unwrap()));
    }

    #[test]
    fn the_name_is_drawn_on_the_picture() {
        let cam = camera(Picture {
            show_name: true,
            ..Picture::default()
        });
        publish(&cam, Frame::black(640, 480));
        let shown = cam.latest_frame().unwrap();
        assert_eq!((shown.width(), shown.height()), (640, 480));
        assert!(shown.y().iter().any(|&y| y > 100), "no text was drawn");
    }

    #[test]
    fn no_signal_is_the_default_when_the_stream_goes() {
        let cam = camera(Picture::default());
        publish(&cam, Frame::black(64, 32));
        assert!(cam.latest_frame().is_some());
        cam.set_bus(None);
        assert!(cam.latest_frame().is_none());
        assert!(!cam.holds_last_frame());
    }

    #[test]
    fn frozen_cameras_keep_the_last_picture() {
        let cam = camera(Picture {
            on_disconnect: OnDisconnect::FreezeLastFrame,
            ..Picture::default()
        });
        assert!(cam.latest_frame().is_none(), "nothing to hold yet");
        publish(&cam, Frame::black(64, 32));
        let last = cam.latest_frame().unwrap();
        cam.set_bus(None);
        assert!(Arc::ptr_eq(&last, &cam.latest_frame().unwrap()));
        // Switched off on purpose: apps should be told, not shown a stale picture.
        cam.set_activity(Activity::Paused);
        assert!(!cam.holds_last_frame());
    }

    #[test]
    fn requested_sizes_map_back_through_rotation_and_crop() {
        let crop = FitMode::Crop;
        let plain = Picture::default();
        assert_eq!(source_size_for(1280, 720, crop, &plain, None), (1280, 720));
        let rotated = Picture {
            rotate: Rotation::Cw90,
            ..Picture::default()
        };
        assert_eq!(
            source_size_for(720, 1280, crop, &rotated, None),
            (1280, 720)
        );
        let cropped = Picture {
            crop: rtspcam_core::config::Crop {
                left: 25,
                right: 25,
                top: 10,
                bottom: 0,
            },
            ..Picture::default()
        };
        // Half the width is shown, so the whole picture must be twice as wide.
        assert_eq!(source_size_for(640, 360, crop, &cropped, None), (1280, 400));
    }

    #[test]
    fn letterboxed_pictures_need_only_the_scale_that_fits() {
        let rotated = Picture {
            rotate: Rotation::Cw90,
            ..Picture::default()
        };
        let source = Some((1920, 1080));
        // A 1080p camera turned upright, shown in 1280x720: 405 rows of picture are enough.
        assert_eq!(
            source_size_for(1280, 720, FitMode::Letterbox, &rotated, source),
            (720, 405)
        );
        // Not knowing the camera's shape yet, ask for enough to be safe.
        assert_eq!(
            source_size_for(1280, 720, FitMode::Letterbox, &rotated, None),
            (720, 1280)
        );
        // Same shape: the output size itself.
        assert_eq!(
            source_size_for(1280, 720, FitMode::Letterbox, &Picture::default(), source),
            (1280, 720)
        );
    }

    #[test]
    fn the_cameras_own_size_is_decoded_without_shrinking() {
        use rtspcam_pipeline::decode::limited_size;
        // A "Source" camera's apps ask for the camera's size (made even and kept within the
        // limits), so the decoder's limit lets whole pictures through.
        for (sw, sh) in [(1920, 1080), (2560, 1440), (1440, 1080), (3840, 2160)] {
            let (w, h) = OutputFormat::source_size(sw, sh).unwrap();
            let cam = camera(Picture::default());
            cam.source_size.set(Some((sw, sh)));
            cam.note_request(w, h, FitMode::Letterbox);
            assert_eq!(cam.size_limit(), Some((sw, sh)));
            assert_eq!(limited_size(sw, sh, cam.size_limit()), (sw, sh));
        }
    }

    #[test]
    fn source_streams_offer_the_size_seen_or_their_own() {
        let mut config = StreamConfig::new("Door", "10.0.0.2");
        config.output = OutputFormat::source(60);
        assert_eq!(offered_output(&config, None), (1920, 1080, 60));
        assert_eq!(
            offered_output(&config, Some((1440, 1080))),
            (1440, 1080, 60)
        );
        // A fixed size ignores what was seen.
        config.output = OutputFormat::default();
        assert_eq!(offered_output(&config, Some((1440, 1080))), (1280, 720, 30));
    }

    #[test]
    fn the_size_limit_is_the_largest_recent_request() {
        let cam = camera(Picture::default());
        assert_eq!(cam.size_limit(), None);
        cam.note_request(640, 360, FitMode::Crop);
        cam.note_request(1280, 720, FitMode::Crop);
        cam.note_request(1920, 400, FitMode::Crop);
        assert_eq!(cam.size_limit(), Some((1920, 720)));
        // Without size limits nothing is recorded.
        let cam = camera(Picture::default()).without_size_limit();
        cam.note_request(640, 360, FitMode::Crop);
        assert_eq!(cam.size_limit(), None);
    }

    /// Moves when `key` was last asked for to `ago` before now.
    fn asked_ago(cam: &CameraShared, key: (u32, u32, FitMode), ago: Duration) {
        let mut r = cam.requests.as_ref().unwrap().lock().unwrap();
        *r.seen.get_mut(&key).unwrap() = std::time::Instant::now().checked_sub(ago).unwrap();
    }

    fn asked_at(cam: &CameraShared, key: (u32, u32, FitMode)) -> std::time::Instant {
        cam.requests.as_ref().unwrap().lock().unwrap().seen[&key]
    }

    #[test]
    fn a_size_asked_for_again_moments_ago_changes_nothing() {
        let cam = camera(Picture::default());
        let key = (1280, 720, FitMode::Crop);
        cam.note_request(key.0, key.1, key.2);
        asked_ago(&cam, key, Duration::from_millis(100));
        let before = asked_at(&cam, key);
        cam.note_request(key.0, key.1, key.2);
        assert_eq!(asked_at(&cam, key), before);
        // Asked for again later: the request is renewed.
        asked_ago(&cam, key, REQUEST_REFRESH + Duration::from_millis(10));
        let before = asked_at(&cam, key);
        cam.note_request(key.0, key.1, key.2);
        assert!(asked_at(&cam, key) > before);
    }

    #[test]
    fn sizes_nobody_asked_for_lately_are_forgotten() {
        let cam = camera(Picture::default());
        cam.note_request(1920, 1080, FitMode::Crop);
        cam.note_request(640, 360, FitMode::Crop);
        assert_eq!(cam.size_limit(), Some((1920, 1080)));
        asked_ago(&cam, (1920, 1080, FitMode::Crop), REQUEST_TTL);
        asked_ago(&cam, (640, 360, FitMode::Crop), REQUEST_REFRESH);
        cam.note_request(640, 360, FitMode::Crop);
        assert_eq!(cam.size_limit(), Some((640, 360)));
    }

    #[test]
    fn the_size_limit_follows_the_cameras_picture_size() {
        let rotated = Picture {
            rotate: Rotation::Cw90,
            ..Picture::default()
        };
        let cam = camera(rotated);
        cam.note_request(1280, 720, FitMode::Letterbox);
        // Not knowing the camera's shape yet: enough for any.
        assert_eq!(cam.size_limit(), Some((720, 1280)));
        // A picture arrived: worked out again at once, though the size was just asked for.
        cam.source_size.set(Some((1920, 1080)));
        cam.note_request(1280, 720, FitMode::Letterbox);
        assert_eq!(cam.size_limit(), Some((720, 405)));
    }
}
