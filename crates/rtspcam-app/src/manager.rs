//! The camera manager: one pipeline, one pipe server and one virtual camera per enabled stream,
//! kept in step with the configuration.
//!
//! ```text
//!  Config ──apply──► reconciler task ──► per camera:
//!                                          ├─ pipe server   (serves the DLL; counts consumers)
//!                                          ├─ supervisor    (starts/stops the RTSP pipeline)
//!                                          └─ virtual camera (via the CameraBackend)
//! ```
//!
//! [`CameraManager::apply`] never blocks: it hands the new configuration to a task that works
//! out what changed and does the slow parts (creating cameras can take a second) in order.
//! Streams that changed are rebuilt from scratch; the rest are left alone.
//!
//! On-demand streams connect only while an app uses the webcam or a [`Preview`] is open, and
//! disconnect [`idle_grace`](ManagerOptions::idle_grace) after the last one leaves.

use std::collections::HashMap;
use std::io;
use std::sync::{Arc, Mutex, PoisonError, RwLock};
use std::thread;
use std::time::Duration;

use rtspcam_core::config::{OnDisconnect, Picture, StreamConfig};
use rtspcam_core::{Config, FitMode};
use rtspcam_ipc::server::serve;
use rtspcam_pipeline::transform;
use rtspcam_pipeline::{
    Frame, FrameBus, Pipeline, PipelineError, PipelineOptions, SourceOptions, StreamState,
};
use tokio::runtime::{Handle, Runtime};
use tokio::sync::watch;
use tokio::task::JoinHandle;
use tokio::time::Instant;
use uuid::Uuid;

use crate::backend::CameraBackend;
use crate::overlay::{Overlay, local_time_text};
use crate::source::CameraSource;
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
}

impl Default for ManagerOptions {
    fn default() -> Self {
        Self {
            idle_grace: Duration::from_secs(10),
            on_change: Arc::new(|| {}),
        }
    }
}

impl std::fmt::Debug for ManagerOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ManagerOptions")
            .field("idle_grace", &self.idle_grace)
            .finish_non_exhaustive()
    }
}

/// How many things want a camera's pictures.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Demand {
    clients: usize,
    previews: usize,
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
            notify,
        }
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

    fn set_vcam(&self, state: VcamState) {
        *self.vcam.lock().unwrap_or_else(PoisonError::into_inner) = state;
        (self.notify)();
    }

    /// Records the outcome of creating the camera, unless something already went wrong.
    fn vcam_created(&self, result: Result<(), String>) {
        {
            let mut vcam = self.vcam.lock().unwrap_or_else(PoisonError::into_inner);
            if *vcam == VcamState::Pending {
                *vcam = match result {
                    Ok(()) => VcamState::Ready,
                    Err(e) => VcamState::Failed(e),
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
    server: JoinHandle<()>,
    supervisor: JoinHandle<()>,
}

struct Shared {
    cameras: Mutex<HashMap<Uuid, Camera>>,
    paused: watch::Sender<bool>,
    backend: Arc<dyn CameraBackend>,
    options: ManagerOptions,
    handle: Handle,
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
    pub fn start(backend: Arc<dyn CameraBackend>, options: ManagerOptions) -> io::Result<Self> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .thread_name("rtspcam")
            .build()?;
        let shared = Arc::new(Shared {
            cameras: Mutex::default(),
            paused: watch::Sender::new(false),
            backend,
            options,
            handle: runtime.handle().clone(),
        });
        let (config_tx, config_rx) = watch::channel(Arc::new(Config::default()));
        let reconciler = runtime.spawn(reconcile_loop(shared.clone(), config_rx));
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
    pub fn apply(&self, config: &Config) {
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
    /// 1. stop configuration changes and the pipe servers (no new consumers),
    /// 2. remove every virtual camera (they disappear from Discord and the like),
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
            c.server.abort();
            c.supervisor.abort();
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

async fn reconcile_loop(shared: Arc<Shared>, mut rx: watch::Receiver<Arc<Config>>) {
    while rx.changed().await.is_ok() {
        let config = rx.borrow_and_update().clone();
        reconcile(&shared, &config).await;
    }
}

async fn reconcile(shared: &Arc<Shared>, config: &Config) {
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
        tracing::info!(id = %camera.state.id, name = %camera.state.name, "removing camera");
        remove_camera(shared, camera).await;
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

async fn remove_camera(shared: &Arc<Shared>, camera: Camera) {
    camera.server.abort();
    camera.supervisor.abort();
    camera.state.set_bus(None);
    if let Some(pipeline) = camera.state.take_pipeline() {
        let _ = tokio::task::spawn_blocking(move || pipeline.stop()).await;
    }
    let backend = shared.backend.clone();
    let id = camera.state.id;
    let _ = tokio::task::spawn_blocking(move || backend.remove(id)).await;
    (shared.options.on_change)();
}

async fn add_camera(shared: &Arc<Shared>, config: &StreamConfig) {
    tracing::info!(id = %config.id, name = %config.name, url = %config.url(), "adding camera");
    let state = Arc::new(CameraShared::new(config, shared.options.on_change.clone()));

    let source = Arc::new(CameraSource::new(state.clone()));
    let server = {
        let state = state.clone();
        let id = config.id;
        shared.handle.spawn(async move {
            if let Err(e) = serve(id, source).await {
                tracing::error!(%id, error = %e, "the camera's pipe stopped");
                state.set_vcam(VcamState::Failed(format!(
                    "could not serve the camera's pipe ({e}); is another copy of RTSP Cam running?"
                )));
            }
        })
    };

    let supervisor = shared.handle.spawn(supervise(
        state.clone(),
        SourceOptions::from_stream(config),
        config.on_demand,
        shared.paused.subscribe(),
        shared.options.idle_grace,
    ));

    shared
        .cameras
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .insert(
            config.id,
            Camera {
                config: config.clone(),
                state: state.clone(),
                server,
                supervisor,
            },
        );
    (shared.options.on_change)();

    // Creating the camera is the slow part; the pipe is already being served.
    let backend = shared.backend.clone();
    let (name, id) = (config.name.clone(), config.id);
    let preferred = (config.output.width, config.output.height, config.output.fps);
    let result = tokio::task::spawn_blocking(move || backend.create(&name, id, preferred))
        .await
        .unwrap_or_else(|e| Err(e.to_string()));
    if let Err(e) = &result {
        tracing::error!(%id, error = %e, "could not create the virtual camera");
    }
    state.vcam_created(result);
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
                *camera
                    .pipeline
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner) = Some(pipeline);
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
    use rtspcam_core::config::Rotation;

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
}
