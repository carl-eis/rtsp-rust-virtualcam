//! The camera manager against a fake Windows backend.
//!
//! Tests that need pictures use the `tools/test-rtsp` server and only run when
//! `RTSPCAM_TEST_SERVER` is set (for example `127.0.0.1:8554`). The rest always run.

use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use rtspcam_app::backend::{CameraBackend, pipe_exists};
use rtspcam_app::{Activity, CameraManager, CameraStatus, ManagerOptions, VcamState};
use rtspcam_core::Config;
use rtspcam_core::config::{Protocol, StreamConfig};

use rtspcam_ipc::client::FrameClient;
use rtspcam_ipc::{Message, PixelFormat, StreamStatus, VideoFormat};
use uuid::Uuid;

const FORMAT: VideoFormat = VideoFormat {
    width: 640,
    height: 480,
    fps: 30,
    pixel_format: PixelFormat::Nv12,
};

/// Records what the manager asked Windows to do.
#[derive(Default)]
struct FakeBackend {
    log: Mutex<Vec<String>>,
    fail_with: Mutex<Option<String>>,
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
}

impl CameraBackend for FakeBackend {
    fn create(&self, name: &str, _id: Uuid, preferred: (u32, u32, u32)) -> Result<(), String> {
        self.push(format!(
            "create {name} {}x{}@{}",
            preferred.0, preferred.1, preferred.2
        ));
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
    }

    fn remove_all(&self) {
        self.push("remove_all".to_owned());
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

/// Reads messages until a frame arrives; returns the statuses seen on the way.
fn read_until_frame(client: &mut FrameClient, timeout: Duration) -> Vec<StreamStatus> {
    let start = Instant::now();
    let mut statuses = Vec::new();
    loop {
        match client.read().expect("pipe read") {
            Message::Frame { data, .. } => {
                assert_eq!(data.len(), FORMAT.frame_len());
                return statuses;
            }
            Message::Status { status, .. } => statuses.push(status),
            other => panic!("unexpected {other:?}"),
        }
        assert!(start.elapsed() < timeout, "no frame within {timeout:?}");
    }
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
    *backend.fail_with.lock().unwrap() = Some("not registered".to_owned());
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
fn shutdown_removes_cameras_and_pipes_and_is_prompt() {
    let backend = Arc::new(FakeBackend::default());
    let m = manager(&backend, Duration::from_millis(200));
    let s = StreamConfig::new("Cam", "192.0.2.10");
    let id = s.id;
    m.apply(&config(vec![s]));
    wait(Duration::from_secs(5), || {
        (m.status(id)?.vcam == VcamState::Ready).then_some(())
    });
    assert!(pipe_exists(id), "pipe is served");

    // A consumer is connected when we quit.
    let mut client = FrameClient::connect(id, FORMAT).expect("connect");
    let _ = client.read();

    let start = Instant::now();
    m.shutdown(Duration::from_secs(5));
    assert!(
        start.elapsed() < Duration::from_secs(3),
        "{:?}",
        start.elapsed()
    );
    assert_eq!(backend.log().last().map(String::as_str), Some("remove_all"));
    wait(Duration::from_secs(2), || (!pipe_exists(id)).then_some(()));
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
    let mut client = FrameClient::connect(id, FORMAT).expect("connect");
    let statuses = loop {
        if let Message::Status { status, .. } = client.read().expect("read") {
            break status;
        }
    };
    assert_eq!(statuses, StreamStatus::Disabled);
    m.set_paused(false);
    // On demand with a consumer connected: it connects (and fails, there is no server).
    wait(Duration::from_secs(5), || {
        matches!(status_of(&m, id).activity, Activity::Running(_)).then_some(())
    });
    drop(client);
    m.shutdown(Duration::from_secs(5));
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

    let mut client = FrameClient::connect(id, FORMAT).expect("connect");
    read_until_frame(&mut client, Duration::from_secs(15));
    // The state follows the first once-a-second statistics, shortly after the first picture.
    let status = wait(Duration::from_secs(5), || {
        let s = status_of(&m, id);
        s.activity.is_streaming().then_some(s)
    });
    assert_eq!(status.clients, 1);

    client.close();
    wait(Duration::from_secs(2), || {
        (status_of(&m, id).clients == 0).then_some(())
    });
    // Still connected during the grace period, idle after it.
    assert!(status_of(&m, id).activity.is_streaming());
    wait(Duration::from_secs(5), || {
        (status_of(&m, id).activity == Activity::Idle).then_some(())
    });

    // A second consumer connects again.
    let mut again = FrameClient::connect(id, FORMAT).expect("reconnect");
    read_until_frame(&mut again, Duration::from_secs(15));
    drop(again);
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
