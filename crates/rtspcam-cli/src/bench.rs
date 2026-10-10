//! `bench`: where the CPU goes with N streams, each read by consumers like a call app would.
//!
//! Runs the app's camera manager (the same pipeline, picture adjustments, scaling and
//! conversion) with a backend whose "cameras" are threads in this process that read frames at
//! the consumer's frame rate. With `--pipe` they read through the OS frame transport (a named
//! pipe on Windows, a Unix socket elsewhere), so the pipe write is measured too. Prints the
//! process CPU and the time per stage (see `rtspcam_core::profile`) as Markdown tables.

use std::collections::HashMap;
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use anyhow::{Context as _, bail};
use clap::Args;
use rtspcam_core::config::{ConfigStore, Crop, Rotation, parse_stream_url};
use rtspcam_core::profile::{self, Stage};
use rtspcam_core::{Config, Secret, StreamConfig};
use rtspcam_engine::{Activity, CameraManager, ManagerOptions};
use rtspcam_ipc::client::FrameClient;
use rtspcam_ipc::protocol::Message;
use rtspcam_ipc::{FrameSource, PixelFormat, VideoFormat};
use rtspcam_pipeline::{StreamState, StreamStats};
use rtspcam_platform::{CameraError, CameraSpec, VirtualCameraBackend};
use tokio::runtime::Handle;
use uuid::Uuid;

use crate::rtsp::{parse_on_off, parse_size, wait_for_probe};

#[derive(Debug, Args)]
pub(crate) struct BenchArgs {
    /// URLs or config stream names.
    #[arg(required = true)]
    streams: Vec<String>,
    /// Consumers per stream (apps reading the camera).
    #[arg(long, default_value_t = 1)]
    consumers: u32,
    /// What each consumer asks for.
    #[arg(long, default_value = "1280x720", value_parser = parse_size)]
    size: (u32, u32),
    #[arg(long, default_value_t = 30)]
    fps: u32,
    /// nv12, rgb32, yuyv or i420.
    #[arg(long, default_value = "nv12", value_parser = parse_pixel)]
    pixel: PixelFormat,
    /// Read through the OS frame transport (named pipe / Unix socket) instead of calling the
    /// frame source directly.
    #[arg(long)]
    pipe: bool,
    /// Hardware decoding: on or off.
    #[arg(long, default_value = "off", value_parser = parse_on_off, action = clap::ArgAction::Set)]
    hardware: bool,
    /// Let the decoder shrink pictures to what the consumers need (the app always does).
    #[arg(long, default_value = "on", value_parser = parse_on_off, action = clap::ArgAction::Set)]
    limit: bool,
    /// Rotate every picture (0, 90, 180, 270), to measure the adjust stage.
    #[arg(long, default_value_t = 0)]
    rotate: u32,
    /// Crop this percentage from every side, to measure the adjust stage.
    #[arg(long, default_value_t = 0)]
    crop: u8,
    /// Draw the stream's name on every picture.
    #[arg(long)]
    show_name: bool,
    /// Seconds to wait for every stream to run before measuring.
    #[arg(long, default_value_t = 5)]
    warmup: u64,
    /// Seconds to measure.
    #[arg(long, default_value_t = 20)]
    seconds: u64,
}

fn parse_pixel(s: &str) -> Result<PixelFormat, String> {
    match s.to_ascii_lowercase().as_str() {
        "nv12" => Ok(PixelFormat::Nv12),
        "rgb32" | "bgra" => Ok(PixelFormat::Rgb32),
        "yuyv" | "yuy2" => Ok(PixelFormat::Yuyv),
        "i420" => Ok(PixelFormat::I420),
        _ => Err("expected nv12, rgb32, yuyv or i420".into()),
    }
}

pub(crate) fn run(store: &ConfigStore, args: BenchArgs) -> anyhow::Result<ExitCode> {
    let mut config = Config::default();
    config.settings.hardware_decoding = args.hardware;
    for s in &args.streams {
        let mut stream = resolve(store, s)?;
        stream.enabled = true;
        stream.on_demand = false;
        stream.output.width = args.size.0;
        stream.output.height = args.size.1;
        stream.output.fps = args.fps;
        stream.picture.rotate = match args.rotate {
            0 => Rotation::None,
            90 => Rotation::Cw90,
            180 => Rotation::Cw180,
            270 => Rotation::Cw270,
            r => bail!("--rotate {r}: expected 0, 90, 180 or 270"),
        };
        stream.picture.crop = Crop {
            left: args.crop,
            top: args.crop,
            right: args.crop,
            bottom: args.crop,
        };
        stream.picture.show_name = args.show_name;
        // Two streams of the same URL are two cameras.
        if config.streams.iter().any(|o| o.name == stream.name) {
            stream.name = format!("{} #{}", stream.name, config.streams.len() + 1);
        }
        config.streams.push(stream);
    }

    let hardware = wait_for_probe();
    println!("hardware decoding: {}", hardware.describe());
    println!(
        "hardware decoding is {} for this run",
        if args.hardware { "on" } else { "off" }
    );

    let format = VideoFormat {
        width: args.size.0,
        height: args.size.1,
        fps: args.fps,
        pixel_format: args.pixel,
    };
    let backend = Arc::new(BenchBackend {
        format,
        consumers: args.consumers,
        pipe: args.pipe,
        cameras: Mutex::default(),
    });
    let options = ManagerOptions {
        limit_decoded_size: args.limit,
        ..ManagerOptions::default()
    };
    let manager = CameraManager::start(backend.clone(), options)?;
    manager.apply(&config);

    // Warm up: every stream streaming, then let it settle.
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let statuses = manager.statuses();
        let streaming = statuses
            .iter()
            .filter(|s| matches!(&s.activity, Activity::Running(StreamState::Streaming(_))))
            .count();
        if statuses.len() == config.streams.len() && streaming == statuses.len() {
            break;
        }
        if Instant::now() > deadline {
            for s in &statuses {
                println!("  {}: {:?}", s.name, s.activity);
            }
            bail!("not every stream started within 30 s");
        }
        thread::sleep(Duration::from_millis(200));
    }
    thread::sleep(Duration::from_secs(args.warmup));

    // Measure.
    let _ = profile::take();
    backend.take_received();
    let cpu_start = rtspcam_platform::desktop::process_cpu_time();
    let started = Instant::now();
    let mut stats: HashMap<String, Vec<StreamStats>> = HashMap::new();
    while started.elapsed() < Duration::from_secs(args.seconds) {
        thread::sleep(Duration::from_secs(1));
        for s in manager.statuses() {
            if let Activity::Running(StreamState::Streaming(st)) = s.activity {
                stats.entry(s.name).or_default().push(st);
            }
        }
    }
    let wall = started.elapsed();
    let cpu = rtspcam_platform::desktop::process_cpu_time()
        .zip(cpu_start)
        .map(|(end, start)| end.saturating_sub(start));
    let stages = profile::take();
    let received = backend.take_received();
    let memory = rtspcam_platform::desktop::process_memory();
    manager.shutdown(Duration::from_secs(5));

    report(
        &args, format, wall, cpu, &stages, &stats, received, memory, &config,
    );
    Ok(ExitCode::SUCCESS)
}

fn resolve(store: &ConfigStore, stream: &str) -> anyhow::Result<StreamConfig> {
    if !stream.contains("://") {
        let config = store.load()?;
        return config
            .streams
            .iter()
            .find(|s| s.name.trim().eq_ignore_ascii_case(stream.trim()))
            .cloned()
            .with_context(|| {
                format!("no stream named \"{stream}\" in {}", store.path().display())
            });
    }
    let url = parse_stream_url(stream)?;
    let name = url.path.trim_start_matches('/').to_owned();
    let name = if name.is_empty() {
        url.host.clone()
    } else {
        name
    };
    let mut s = StreamConfig::new(name, url.host);
    s.protocol = url.protocol;
    s.port = url.port;
    s.path = url.path;
    s.username = url.username;
    s.password = url.password.map(Secret::new);
    Ok(s)
}

#[allow(clippy::too_many_arguments)]
fn report(
    args: &BenchArgs,
    format: VideoFormat,
    wall: Duration,
    cpu: Option<Duration>,
    stages: &[profile::StageTotal],
    stats: &HashMap<String, Vec<StreamStats>>,
    received: u64,
    memory: Option<(usize, usize)>,
    config: &Config,
) {
    let secs = wall.as_secs_f64();
    let pct = |d: Duration| d.as_secs_f64() / secs * 100.0;
    println!();
    println!(
        "{} stream(s), {} consumer(s) each at {format}{}{}, hardware decoding {}, {:.0} s",
        config.streams.len(),
        args.consumers,
        if args.pipe { " over the pipe" } else { "" },
        if args.limit { "" } else { ", no size limit" },
        if args.hardware { "on" } else { "off" },
        secs
    );
    println!();
    println!("| Stream | Decoder | Camera size | Decoded size | fps | Decode ms/picture |");
    println!("|---|---|---|---|---|---|");
    let mut names: Vec<&String> = stats.keys().collect();
    names.sort();
    for name in names {
        let s = &stats[name];
        let Some(last) = s.last() else { continue };
        let n = s.len() as f64;
        let fps = s.iter().map(|x| f64::from(x.fps)).sum::<f64>() / n;
        let decode = s.iter().map(|x| x.decode_time.as_secs_f64()).sum::<f64>() / n;
        println!(
            "| {name} | {} | {}x{} | {}x{} | {fps:.1} | {:.2} |",
            last.decoder,
            last.width,
            last.height,
            last.decoded_size.0,
            last.decoded_size.1,
            decode * 1000.0
        );
    }
    println!();
    println!("| Stage | CPU % of one core | Calls/s | ms per call |");
    println!("|---|---|---|---|");
    for t in stages.iter().filter(|t| t.calls > 0) {
        println!(
            "| {} | {:.1} | {:.0} | {:.2} |",
            t.stage.name(),
            pct(t.time),
            t.calls as f64 / secs,
            t.per_call().as_secs_f64() * 1000.0
        );
    }
    println!();
    match cpu {
        Some(cpu) => {
            let cores = thread::available_parallelism().map_or(1, |n| n.get());
            println!(
                "process CPU: {:.1} % of one core ({:.1} % per stream; {:.2} % of all {cores} logical cores per stream)",
                pct(cpu),
                pct(cpu) / config.streams.len() as f64,
                pct(cpu) / config.streams.len() as f64 / cores as f64
            );
        }
        None => println!("process CPU: not available on this OS"),
    }
    println!(
        "frames read by consumers: {:.1} per second per consumer",
        received as f64 / secs / f64::from(args.consumers.max(1)) / config.streams.len() as f64
    );
    if let Some((ws, private)) = memory {
        println!(
            "memory: {} MB working set, {} MB private",
            ws / (1024 * 1024),
            private / (1024 * 1024)
        );
    }
    let deliver = stages.iter().find(|t| t.stage == Stage::Deliver);
    if args.pipe && deliver.is_some_and(|d| d.calls > 0) {
        println!("(deliver includes waiting for the reading thread)");
    }
}

/// "Cameras" that are reader threads in this process.
struct BenchBackend {
    format: VideoFormat,
    consumers: u32,
    pipe: bool,
    cameras: Mutex<HashMap<Uuid, BenchCamera>>,
}

struct BenchCamera {
    stop: Arc<AtomicBool>,
    readers: Vec<JoinHandle<()>>,
    server: Option<tokio::task::JoinHandle<()>>,
    received: Arc<AtomicU64>,
}

impl BenchCamera {
    fn stop(self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(server) = self.server {
            server.abort();
        }
        for r in self.readers {
            let _ = r.join();
        }
    }
}

impl BenchBackend {
    fn take_received(&self) -> u64 {
        self.cameras
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .values()
            .map(|c| c.received.swap(0, Ordering::Relaxed))
            .sum()
    }
}

impl VirtualCameraBackend for BenchBackend {
    fn check(&self) -> Result<(), CameraError> {
        Ok(())
    }

    fn create(
        &self,
        spec: &CameraSpec,
        frames: Arc<dyn FrameSource>,
        runtime: &Handle,
    ) -> Result<(), CameraError> {
        let stop = Arc::new(AtomicBool::new(false));
        let received = Arc::new(AtomicU64::new(0));
        let failed = |e: std::io::Error| CameraError::Failed(e.to_string());
        let mut server = None;
        let transport = rtspcam_platform::frame_transport();
        if self.pipe {
            let _guard = runtime.enter();
            let listener = transport.listen(spec.id).map_err(failed)?;
            let frames = frames.clone();
            server = Some(runtime.spawn(async move {
                let _ = rtspcam_ipc::server::serve(listener, frames).await;
            }));
        }
        let mut readers = Vec::new();
        for i in 0..self.consumers {
            let (stop, received, format) = (stop.clone(), received.clone(), self.format);
            let name = format!("consumer {} {i}", spec.name);
            let reader = if self.pipe {
                let stream = transport.connect(spec.id).map_err(failed)?;
                let mut client = FrameClient::new(stream, format).map_err(failed)?;
                thread::Builder::new().name(name).spawn(move || {
                    while !stop.load(Ordering::SeqCst) {
                        match client.read() {
                            Ok(Message::Frame { .. }) => {
                                received.fetch_add(1, Ordering::Relaxed);
                            }
                            Ok(_) => {}
                            Err(_) => break,
                        }
                    }
                })
            } else {
                // Like the pipe server: one slot per frame interval, taking only new pictures,
                // and waiting up to half a slot for a late one.
                let frames = frames.clone();
                thread::Builder::new().name(name).spawn(move || {
                    frames.client_connected(format);
                    let interval = Duration::from_secs(1) / format.fps.max(1);
                    let (mut out, mut last) = (Vec::new(), None);
                    let mut slot = Instant::now();
                    while !stop.load(Ordering::SeqCst) {
                        let late_until = slot + interval / 2;
                        loop {
                            if let Some(seq) = frames.next_frame(format, last, &mut out) {
                                last = Some(seq);
                                received.fetch_add(1, Ordering::Relaxed);
                                break;
                            }
                            if Instant::now() >= late_until {
                                break;
                            }
                            thread::sleep(Duration::from_millis(3));
                        }
                        slot += interval;
                        thread::sleep(slot.saturating_duration_since(Instant::now()));
                    }
                    frames.client_disconnected();
                })
            };
            readers.push(reader.map_err(failed)?);
        }
        self.cameras
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(
                spec.id,
                BenchCamera {
                    stop,
                    readers,
                    server,
                    received,
                },
            );
        Ok(())
    }

    fn remove(&self, id: Uuid) {
        let camera = self
            .cameras
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&id);
        if let Some(c) = camera {
            c.stop();
        }
    }

    fn remove_all(&self) {
        let cameras: Vec<BenchCamera> = self
            .cameras
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .drain()
            .map(|(_, c)| c)
            .collect();
        for c in cameras {
            c.stop();
        }
    }
}
