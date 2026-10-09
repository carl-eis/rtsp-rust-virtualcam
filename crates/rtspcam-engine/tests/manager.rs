//! The camera manager against a fake camera backend, so it runs on every OS without any camera
//! support installed. The fake backend keeps each camera's `FrameSource`; tests play the
//! consumer (Discord, the Camera app, ...) by calling it directly.
//!
//! Tests that need pictures use the `tools/test-rtsp` server and only run when
//! `RTSPCAM_TEST_SERVER` is set (for example `127.0.0.1:8554`). The rest always run.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use rtspcam_core::Config;
use rtspcam_core::config::{Protocol, StreamConfig};
use rtspcam_engine::{Activity, CameraManager, CameraStatus, ManagerOptions, VcamState};
use rtspcam_ipc::client::FrameClient;
use rtspcam_ipc::server::serve;
use rtspcam_ipc::{FrameSource, Message, PixelFormat, StreamStatus, VideoFormat};
use rtspcam_platform::{CameraError, CameraSpec, FrameTransport, VirtualCameraBackend};
use tokio::runtime::Handle;
use uuid::Uuid;

const FORMAT: VideoFormat = VideoFormat {
    width: 640,
    height: 480,
    fps: 30,
    pixel_format: PixelFormat::Nv12,
};

/// Records what the manager asked for, and keeps the cameras' frame sources.
#[derive(Default)]
struct FakeBackend {
    log: Mutex<Vec<String>>,
    fail_with: Mutex<Option<CameraError>>,
    sources: Mutex<HashMap<Uuid, Arc<dyn FrameSource>>>,
}

impl FakeBackend {
    fn log(&self) -> Vec<String> {
        self.log
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    fn push(&self, entry: String) {
        self.log
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(entry);
    }

    /// What a consumer of camera `id` talks to.
    fn source(&self, id: Uuid) -> Arc<dyn FrameSource> {
        self.sources
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&id)
            .cloned()
            .expect("the camera was created")
    }

    fn has_source(&self, id: Uuid) -> bool {
        self.sources
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .contains_key(&id)
    }
}

impl VirtualCameraBackend for FakeBackend {
    fn check(&self) -> Result<(), CameraError> {
        Ok(())
    }

    fn create(
        &self,
        spec: &CameraSpec,
        frames: Arc<dyn FrameSource>,
        _runtime: &Handle,
    ) -> Result<(), CameraError> {
        let (w, h, fps) = spec.preferred;
        self.push(format!("create {} {w}x{h}@{fps}", spec.name));
        self.sources
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(spec.id, frames);
        match self
            .fail_with
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
        {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }

    fn remove(&self, id: Uuid) {
        self.push(format!("remove {id}"));
        self.sources
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&id);
    }

    fn remove_all(&self) {
        self.push("remove_all".to_owned());
        self.sources
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clear();
    }
}

fn manager(backend: &Arc<FakeBackend>, grace: Duration) -> CameraManager {
    let options = ManagerOptions {
        idle_grace: grace,
        ..ManagerOptions::default()
    };
    CameraManager::start(backend.clone(), options).expect("manager starts")
}

fn config(streams: Vec<StreamConfig>) -> Config {
    Config {
        streams,
        ..Config::default()
    }
}

fn stream(name: &str, url_path: &str, server: &str) -> StreamConfig {
    let (host, port) = server.rsplit_once(':').expect("host:port");
    let mut s = StreamConfig::new(name, host);
    s.port = port.parse().expect("port");
    s.path = url_path.to_owned();
    s
}

fn wait<T>(timeout: Duration, mut f: impl FnMut() -> Option<T>) -> T {
    let start = Instant::now();
    loop {
        if let Some(v) = f() {
            return v;
        }
        assert!(start.elapsed() < timeout, "timed out after {timeout:?}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn status_of(m: &CameraManager, id: Uuid) -> CameraStatus {
    m.status(id).expect("camera exists")
}

fn server() -> Option<String> {
    match std::env::var("RTSPCAM_TEST_SERVER") {
        Ok(s) if !s.trim().is_empty() => Some(s.trim().to_owned()),
        _ => {
            eprintln!("RTSPCAM_TEST_SERVER not set; skipping");
            None
        }
    }
}

/// Asks a camera's source for frames like a consumer at 30 fps would, until one arrives.
fn read_until_frame(source: &dyn FrameSource, timeout: Duration) {
    let mut out = Vec::new();
    wait(timeout, || {
        let got = source.next_frame(FORMAT, None, &mut out);
        if got.is_none() {
            std::thread::sleep(Duration::from_millis(33));
        }
        got
    });
    assert_eq!(out.len(), FORMAT.frame_len());
}

#[test]
fn creates_cameras_for_enabled_streams_only() {
    let backend = Arc::new(FakeBackend::default());
    let m = manager(&backend, Duration::from_millis(200));
    let mut on = StreamConfig::new("Front Door", "192.0.2.10");
    on.output.width = 1920;
    on.output.height = 1080;
    on.output.fps = 15;
    let mut off = StreamConfig::new("Garage", "192.0.2.11");
    off.enabled = false;
    let (on_id, off_id) = (on.id, off.id);
    m.apply(&config(vec![on, off]));

    wait(Duration::from_secs(5), || {
        (m.status(on_id)?.vcam == VcamState::Ready).then_some(())
    });
    assert_eq!(backend.log(), ["create Front Door 1920x1080@15"]);
    assert!(m.status(off_id).is_none());
    // On demand and unused: not connected.
    assert_eq!(status_of(&m, on_id).activity, Activity::Idle);
    m.shutdown(Duration::from_secs(5));
}

#[test]
fn camera_failures_are_reported() {
    let backend = Arc::new(FakeBackend::default());
    *backend.fail_with.lock().unwrap() = Some(CameraError::Failed("not registered".to_owned()));
    let m = manager(&backend, Duration::from_millis(200));
    let s = StreamConfig::new("Cam", "192.0.2.10");
    let id = s.id;
    m.apply(&config(vec![s]));
    let status = wait(Duration::from_secs(5), || {
        let s = m.status(id)?;
        (s.vcam != VcamState::Pending).then_some(s)
    });
    assert_eq!(status.vcam, VcamState::Failed("not registered".to_owned()));
    assert!(status.has_problem());
    m.shutdown(Duration::from_secs(5));
}

#[test]
fn no_camera_support_is_a_status_not_a_problem() {
    let backend = Arc::new(FakeBackend::default());
    *backend.fail_with.lock().unwrap() = Some(CameraError::Unsupported(
        rtspcam_platform::UNSUPPORTED_MESSAGE.to_owned(),
    ));
    let m = manager(&backend, Duration::from_millis(200));
    let s = StreamConfig::new("Cam", "192.0.2.10");
    let id = s.id;
    m.apply(&config(vec![s]));
    let status = wait(Duration::from_secs(5), || {
        let s = m.status(id)?;
        (s.vcam != VcamState::Pending).then_some(s)
    });
    assert_eq!(
        status.vcam,
        VcamState::Unsupported(rtspcam_platform::UNSUPPORTED_MESSAGE.to_owned())
    );
    assert!(!status.has_problem(), "{status:?}");
    // The stream itself still works: a preview can open it.
    assert!(m.open_preview(id).is_some());
    m.shutdown(Duration::from_secs(5));
}

#[test]
fn unsupported_protocol_is_an_error_not_a_crash() {
    let backend = Arc::new(FakeBackend::default());
    let m = manager(&backend, Duration::from_millis(200));
    let mut s = StreamConfig::new("Odd", "192.0.2.10");
    s.protocol = Protocol::Unsupported("http".to_owned());
    let id = s.id;
    m.apply(&config(vec![s]));
    let status = wait(Duration::from_secs(5), || {
        let s = m.status(id)?;
        matches!(s.activity, Activity::Invalid(_)).then_some(s)
    });
    assert!(status.activity.summary().contains("http"), "{status:?}");
    m.shutdown(Duration::from_secs(5));
}

#[test]
fn edits_rebuild_only_what_changed_and_removals_remove() {
    let backend = Arc::new(FakeBackend::default());
    let m = manager(&backend, Duration::from_millis(200));
    let a = StreamConfig::new("A", "192.0.2.10");
    let b = StreamConfig::new("B", "192.0.2.11");
    let (a_id, b_id) = (a.id, b.id);
    m.apply(&config(vec![a.clone(), b.clone()]));
    wait(Duration::from_secs(5), || {
        (backend.log().len() == 2).then_some(())
    });

    let mut renamed = a.clone();
    renamed.name = "A2".to_owned();
    m.apply(&config(vec![renamed, b.clone()]));
    wait(Duration::from_secs(5), || {
        backend
            .log()
            .contains(&"create A2 1280x720@30".to_owned())
            .then_some(())
    });
    let log = backend.log();
    assert!(log.contains(&format!("remove {a_id}")), "{log:?}");
    assert!(
        !log.contains(&format!("remove {b_id}")),
        "B was touched: {log:?}"
    );

    m.apply(&config(vec![b]));
    wait(Duration::from_secs(5), || {
        m.status(a_id).is_none().then_some(())
    });
    assert!(m.status(b_id).is_some());
    m.shutdown(Duration::from_secs(5));
}

#[test]
fn shutdown_removes_cameras_and_is_prompt() {
    let backend = Arc::new(FakeBackend::default());
    let m = manager(&backend, Duration::from_millis(200));
    let s = StreamConfig::new("Cam", "192.0.2.10");
    let id = s.id;
    m.apply(&config(vec![s]));
    wait(Duration::from_secs(5), || {
        (m.status(id)?.vcam == VcamState::Ready).then_some(())
    });
    // A consumer is using the camera when we quit.
    backend.source(id).client_connected(FORMAT);

    let start = Instant::now();
    m.shutdown(Duration::from_secs(5));
    assert!(
        start.elapsed() < Duration::from_secs(3),
        "{:?}",
        start.elapsed()
    );
    assert_eq!(backend.log().last().map(String::as_str), Some("remove_all"));
    assert!(!backend.has_source(id));
}

#[test]
fn pause_shows_disabled_and_resume_restores() {
    let backend = Arc::new(FakeBackend::default());
    let m = manager(&backend, Duration::from_millis(200));
    let s = StreamConfig::new("Cam", "192.0.2.10");
    let id = s.id;
    m.apply(&config(vec![s]));
    wait(Duration::from_secs(5), || {
        (m.status(id)?.vcam == VcamState::Ready).then_some(())
    });
    m.set_paused(true);
    wait(Duration::from_secs(2), || {
        (status_of(&m, id).activity == Activity::Paused).then_some(())
    });
    let source = backend.source(id);
    source.client_connected(FORMAT);
    assert_eq!(source.status().0, StreamStatus::Disabled);
    m.set_paused(false);
    // On demand with a consumer connected: it connects (and fails, there is no server).
    wait(Duration::from_secs(5), || {
        matches!(status_of(&m, id).activity, Activity::Running(_)).then_some(())
    });
    source.client_disconnected();
    m.shutdown(Duration::from_secs(5));
}

/// Serves each camera over this OS's real transport, like the Windows backend does for the
/// DLL, so the manager's frames are checked end to end through the IPC protocol.
struct ServingBackend {
    transport: Arc<dyn FrameTransport>,
    servers: Mutex<HashMap<Uuid, tokio::task::JoinHandle<()>>>,
}

impl VirtualCameraBackend for ServingBackend {
    fn check(&self) -> Result<(), CameraError> {
        Ok(())
    }

    fn create(
        &self,
        spec: &CameraSpec,
        frames: Arc<dyn FrameSource>,
        runtime: &Handle,
    ) -> Result<(), CameraError> {
        let listener = {
            let _enter = runtime.enter();
            self.transport.listen(spec.id)
        }
        .map_err(|e| CameraError::Failed(e.to_string()))?;
        let task = runtime.spawn(async move {
            let _ = serve(listener, frames).await;
        });
        self.servers.lock().unwrap().insert(spec.id, task);
        Ok(())
    }

    fn remove(&self, id: Uuid) {
        if let Some(t) = self.servers.lock().unwrap().remove(&id) {
            t.abort();
        }
    }

    fn remove_all(&self) {
        for (_, t) in self.servers.lock().unwrap().drain() {
            t.abort();
        }
    }
}

#[test]
fn a_consumer_over_the_platform_transport_sees_the_camera_state() {
    let transport = rtspcam_platform::frame_transport();
    let backend = Arc::new(ServingBackend {
        transport: transport.clone(),
        servers: Mutex::default(),
    });
    let m = CameraManager::start(backend, ManagerOptions::default()).expect("manager starts");
    let s = StreamConfig::new("Cam", "192.0.2.10");
    let id = s.id;
    m.apply(&config(vec![s]));
    wait(Duration::from_secs(5), || {
        (m.status(id)?.vcam == VcamState::Ready).then_some(())
    });
    m.set_paused(true);

    let mut client = FrameClient::new(transport.connect(id).expect("connect"), FORMAT).unwrap();
    let status = loop {
        if let Message::Status { status, .. } = client.read().expect("read") {
            break status;
        }
    };
    assert_eq!(status, StreamStatus::Disabled);
    wait(Duration::from_secs(2), || {
        (status_of(&m, id).clients == 1).then_some(())
    });

    let start = Instant::now();
    m.shutdown(Duration::from_secs(5));
    assert!(start.elapsed() < Duration::from_secs(3));
    wait(Duration::from_secs(2), || {
        (!transport.is_served(id)).then_some(())
    });
}

#[test]
fn on_demand_connects_for_a_consumer_and_disconnects_after_the_grace() {
    let Some(server) = server() else { return };
    let backend = Arc::new(FakeBackend::default());
    let m = manager(&backend, Duration::from_millis(800));
    let s = stream("Cam", "h264-720p", &server);
    let id = s.id;
    m.apply(&config(vec![s]));
    wait(Duration::from_secs(5), || {
        (m.status(id)?.vcam == VcamState::Ready).then_some(())
    });
    assert_eq!(status_of(&m, id).activity, Activity::Idle);

    let source = backend.source(id);
    source.client_connected(FORMAT);
    read_until_frame(&*source, Duration::from_secs(15));
    // The state follows the first once-a-second statistics, shortly after the first picture.
    let status = wait(Duration::from_secs(5), || {
        let s = status_of(&m, id);
        s.activity.is_streaming().then_some(s)
    });
    assert_eq!(status.clients, 1);

    source.client_disconnected();
    wait(Duration::from_secs(2), || {
        (status_of(&m, id).clients == 0).then_some(())
    });
    // Still connected during the grace period, idle after it.
    assert!(status_of(&m, id).activity.is_streaming());
    wait(Duration::from_secs(5), || {
        (status_of(&m, id).activity == Activity::Idle).then_some(())
    });

    // A second consumer connects again.
    source.client_connected(FORMAT);
    read_until_frame(&*source, Duration::from_secs(15));
    source.client_disconnected();
    m.shutdown(Duration::from_secs(5));
}

#[test]
fn a_preview_keeps_an_on_demand_stream_connected() {
    let Some(server) = server() else { return };
    let backend = Arc::new(FakeBackend::default());
    let m = manager(&backend, Duration::from_millis(300));
    let s = stream("Cam", "h264-720p", &server);
    let id = s.id;
    m.apply(&config(vec![s]));
    wait(Duration::from_secs(5), || m.status(id).map(|_| ()));

    let preview = m.open_preview(id).expect("preview");
    wait(Duration::from_secs(15), || preview.latest());
    wait(Duration::from_secs(5), || {
        status_of(&m, id).activity.is_streaming().then_some(())
    });
    std::thread::sleep(Duration::from_millis(800));
    assert!(
        status_of(&m, id).activity.is_streaming(),
        "grace is for idle cameras only"
    );

    drop(preview);
    wait(Duration::from_secs(5), || {
        (status_of(&m, id).activity == Activity::Idle).then_some(())
    });
    m.shutdown(Duration::from_secs(5));
}

#[test]
fn always_on_streams_connect_without_consumers() {
    let Some(server) = server() else { return };
    let backend = Arc::new(FakeBackend::default());
    let m = manager(&backend, Duration::from_millis(300));
    let mut s = stream("Cam", "h264-720p", &server);
    s.on_demand = false;
    let id = s.id;
    m.apply(&config(vec![s]));
    wait(Duration::from_secs(15), || {
        m.status(id)?.activity.is_streaming().then_some(())
    });
    m.shutdown(Duration::from_secs(5));
}

#[test]
fn an_unreachable_camera_shows_no_signal_and_keeps_the_camera() {
    let backend = Arc::new(FakeBackend::default());
    let m = manager(&backend, Duration::from_millis(300));
    // Nothing listens on this port.
    let mut s = StreamConfig::new("Cam", "127.0.0.1");
    s.port = 9;
    s.on_demand = false;
    let id = s.id;
    m.apply(&config(vec![s]));
    let status = wait(Duration::from_secs(10), || {
        let s = m.status(id)?;
        s.activity.is_error().then_some(s)
    });
    assert!(!matches!(status.vcam, VcamState::Failed(_)), "{status:?}");
    assert!(status.activity.hint().is_some(), "{status:?}");
    m.shutdown(Duration::from_secs(5));
}
