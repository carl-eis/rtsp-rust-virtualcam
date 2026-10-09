//! `probe` and `view`: try RTSP streams without the app.

use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, Instant};

use anyhow::{Context as _, bail};
use clap::Args;
use rtspcam_core::config::ConfigStore;
use rtspcam_core::{FitMode, Transport};
use rtspcam_pipeline::decode::{DecoderChoice, create_decoder};
use rtspcam_pipeline::scale::nv12_to_bgra;
use rtspcam_pipeline::{
    Frame, Matrix, Pipeline, PipelineError, PipelineOptions, RtspSource, Scaler, SourceOptions,
    StreamState,
};

/// Which stream(s) to use and how to connect.
#[derive(Debug, Args)]
pub(crate) struct Target {
    /// `rtsp://[user:pass@]host[:port]/path`, or the name of a stream in the config.
    pub(crate) stream: String,
    #[command(flatten)]
    pub(crate) common: Common,
}

#[derive(Debug, Args)]
pub(crate) struct Common {
    /// Override the transport (tcp or udp).
    #[arg(long, value_parser = parse_transport)]
    pub(crate) transport: Option<Transport>,
    /// Decoder: auto, mf (Media Foundation) or openh264.
    #[arg(long, default_value_t = DecoderChoice::Auto)]
    pub(crate) decoder: DecoderChoice,
}

#[derive(Debug, Args)]
pub(crate) struct ProbeArgs {
    #[command(flatten)]
    pub(crate) target: Target,
    /// Give up after this many seconds.
    #[arg(long, default_value_t = 15)]
    pub(crate) timeout: u64,
    /// Save the first decoded picture as a BMP file.
    #[arg(long, value_name = "FILE")]
    pub(crate) snapshot: Option<PathBuf>,
}

#[derive(Debug, Args)]
pub(crate) struct ViewArgs {
    /// URLs or config stream names.
    #[arg(required_unless_present = "all")]
    pub(crate) streams: Vec<String>,
    /// Run every enabled stream in the config.
    #[arg(long)]
    pub(crate) all: bool,
    #[command(flatten)]
    pub(crate) common: Common,
    /// Stop after this many seconds (default: run until Ctrl+C).
    #[arg(long)]
    pub(crate) seconds: Option<u64>,
    /// Seconds between status lines.
    #[arg(long, default_value_t = 1)]
    pub(crate) interval: u64,
    /// Save the latest picture of each stream as BMP into this folder at every status line.
    #[arg(long, value_name = "DIR")]
    pub(crate) dump: Option<PathBuf>,
    /// Size of dumped pictures (the camera output size), e.g. 1280x720.
    #[arg(long, default_value = "1280x720", value_parser = parse_size)]
    pub(crate) size: (u32, u32),
    /// How dumped pictures fit that size: letterbox, crop or stretch.
    #[arg(long, default_value = "letterbox", value_parser = parse_fit)]
    pub(crate) fit: FitMode,
}

fn parse_transport(s: &str) -> Result<Transport, String> {
    match s.to_ascii_lowercase().as_str() {
        "tcp" => Ok(Transport::Tcp),
        "udp" => Ok(Transport::Udp),
        _ => Err("expected tcp or udp".into()),
    }
}

fn parse_fit(s: &str) -> Result<FitMode, String> {
    match s.to_ascii_lowercase().as_str() {
        "letterbox" => Ok(FitMode::Letterbox),
        "crop" => Ok(FitMode::Crop),
        "stretch" => Ok(FitMode::Stretch),
        _ => Err("expected letterbox, crop or stretch".into()),
    }
}

fn parse_size(s: &str) -> Result<(u32, u32), String> {
    let (w, h) = s.split_once(['x', 'X']).ok_or("expected WIDTHxHEIGHT")?;
    let w: u32 = w.parse().map_err(|_| "bad width")?;
    let h: u32 = h.parse().map_err(|_| "bad height")?;
    if w == 0 || h == 0 || w % 2 == 1 || h % 2 == 1 {
        return Err("width and height must be even and non-zero".into());
    }
    Ok((w, h))
}

/// Turns a URL or config stream name into a display name and source options.
fn resolve(
    store: &ConfigStore,
    stream: &str,
    common: &Common,
) -> anyhow::Result<(String, SourceOptions)> {
    let (name, mut opts) = if stream.contains("://") {
        (stream.to_owned(), SourceOptions::from_url(stream)?)
    } else {
        let config = store.load()?;
        let s = config
            .streams
            .iter()
            .find(|s| s.name.trim().eq_ignore_ascii_case(stream.trim()))
            .with_context(|| {
                format!("no stream named \"{stream}\" in {}", store.path().display())
            })?;
        (s.name.clone(), SourceOptions::from_stream(s)?)
    };
    if let Some(t) = common.transport {
        opts.transport = t;
    }
    Ok((name, opts))
}

fn runtime() -> anyhow::Result<tokio::runtime::Runtime> {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("could not start the async runtime")
}

fn print_error(e: &PipelineError) {
    println!("error: {e} ({:?})", e.kind());
    if let Some(hint) = e.kind().hint() {
        println!("hint:  {hint}");
    }
}

pub(crate) fn probe(store: &ConfigStore, args: ProbeArgs) -> anyhow::Result<ExitCode> {
    let (name, mut opts) = resolve(store, &args.target.stream, &args.target.common)?;
    opts.connect_timeout = Duration::from_secs(args.timeout);
    println!("stream:    {name}");
    println!("url:       {}", opts.url);
    let rt = runtime()?;
    let started = Instant::now();
    let result = rt.block_on(async {
        let mut source = RtspSource::connect(&opts).await?;
        let connected = started.elapsed();
        let info = source.info().clone();
        println!("connected: {} ms", connected.as_millis());
        if let Some(tool) = &info.tool {
            println!("server:    {tool}");
        }
        println!(
            "codec:     {}{}",
            info.codec,
            info.codec_string
                .as_deref()
                .map(|s| format!(" ({s})"))
                .unwrap_or_default()
        );
        // Decoding happens on this thread; Media Foundation is fine with that for a probe.
        let deadline = started + Duration::from_secs(args.timeout);
        let mut decoder = None;
        let mut pictures = Vec::new();
        let mut frames = 0u32;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            let frame = tokio::time::timeout(remaining, source.next_frame())
                .await
                .map_err(|_| {
                    PipelineError::new(rtspcam_pipeline::ErrorKind::Timeout, "no picture in time")
                })??
                .ok_or_else(|| {
                    PipelineError::new(rtspcam_pipeline::ErrorKind::EndOfStream, "stream ended")
                })?;
            frames += 1;
            if decoder.is_none() {
                if !frame.keyframe {
                    continue;
                }
                let info = source.info();
                let d = create_decoder(info.codec, info.size, args.target.common.decoder)?;
                println!("decoder:   {}", d.name());
                decoder = Some(d);
            }
            if let Some(d) = decoder.as_mut() {
                d.decode(&frame, &mut pictures)?;
            }
            if let Some(p) = pictures.pop() {
                let info = source.info();
                return Ok::<_, PipelineError>((p, frames, info.fps));
            }
        }
    });
    match result {
        Ok((picture, frames, fps)) => {
            println!("picture:   {}x{}", picture.width(), picture.height());
            if let Some(fps) = fps {
                println!("fps:       {fps:.1} (declared)");
            }
            println!(
                "first picture after {} ms ({frames} frames received)",
                started.elapsed().as_millis()
            );
            if let Some(path) = &args.snapshot {
                save_bmp(path, &picture)?;
                println!("snapshot:  {}", path.display());
            }
            Ok(ExitCode::SUCCESS)
        }
        Err(e) => {
            print_error(&e);
            Ok(ExitCode::FAILURE)
        }
    }
}

pub(crate) fn view(store: &ConfigStore, args: ViewArgs) -> anyhow::Result<ExitCode> {
    let mut targets = Vec::new();
    if args.all {
        let config = store.load()?;
        for s in config.streams.iter().filter(|s| s.enabled) {
            let mut opts = SourceOptions::from_stream(s)?;
            if let Some(t) = args.common.transport {
                opts.transport = t;
            }
            targets.push((s.name.clone(), opts));
        }
        if targets.is_empty() {
            bail!("no enabled streams in {}", store.path().display());
        }
    }
    for s in &args.streams {
        targets.push(resolve(store, s, &args.common)?);
    }
    if let Some(dir) = &args.dump {
        fs::create_dir_all(dir).with_context(|| format!("could not create {}", dir.display()))?;
    }

    let rt = runtime()?;
    let pipelines: Vec<Pipeline> = targets
        .into_iter()
        .map(|(name, source)| {
            let mut opts = PipelineOptions::new(source);
            opts.decoder = args.common.decoder;
            Pipeline::start(name, opts, rt.handle())
        })
        .collect();

    let started = Instant::now();
    let end = args.seconds.map(|s| started + Duration::from_secs(s));
    let interval = Duration::from_secs(args.interval.max(1));
    let mut scaler = Scaler::new();
    let mut tick = 0u64;
    let name_width = pipelines.iter().map(|p| p.name().len()).max().unwrap_or(0);
    loop {
        std::thread::sleep(interval);
        tick += 1;
        let (working_set, private) =
            rtspcam_platform::desktop::process_memory().unwrap_or_default();
        println!(
            "[{:>6.0} s] memory: {} MB working set, {} MB private",
            started.elapsed().as_secs_f64(),
            working_set / (1024 * 1024),
            private / (1024 * 1024)
        );
        for p in &pipelines {
            let state = p.current_status();
            let detail = match &state {
                StreamState::Streaming(s) => format!(
                    " | {:.1} Mbit/s, latency {:.1} ms, {} decoded, {} errors, {} lost | {}",
                    s.bitrate as f64 / 1e6,
                    s.latency.as_secs_f64() * 1000.0,
                    s.frames_decoded,
                    s.decode_errors,
                    s.packets_lost,
                    s.decoder
                ),
                _ => String::new(),
            };
            println!("  {:name_width$}  {}{detail}", p.name(), state.summary());
            if let (Some(dir), Some(frame)) = (&args.dump, p.frames().latest()) {
                let out = scaler.scale(&frame, args.size.0, args.size.1, args.fit);
                let file = dir.join(format!("{}-{tick:05}.bmp", file_safe(p.name())));
                save_bmp(&file, &out)?;
            }
        }
        std::io::stdout().flush().ok();
        if end.is_some_and(|e| Instant::now() >= e) {
            break;
        }
    }
    for p in pipelines {
        p.stop();
    }
    Ok(ExitCode::SUCCESS)
}

fn file_safe(name: &str) -> String {
    let s: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    s.trim_matches('_').chars().take(60).collect()
}

/// Writes a 32-bit top-down BMP.
pub(crate) fn save_bmp(path: &Path, frame: &Frame) -> anyhow::Result<()> {
    let mut bgra = Vec::new();
    nv12_to_bgra(frame, Matrix::for_height(frame.height()), &mut bgra);
    let (w, h) = (frame.width(), frame.height());
    let mut file = Vec::with_capacity(54 + bgra.len());
    file.extend_from_slice(b"BM");
    file.extend_from_slice(&(54 + bgra.len() as u32).to_le_bytes());
    file.extend_from_slice(&[0; 4]);
    file.extend_from_slice(&54u32.to_le_bytes());
    file.extend_from_slice(&40u32.to_le_bytes()); // BITMAPINFOHEADER
    file.extend_from_slice(&(w as i32).to_le_bytes());
    file.extend_from_slice(&(-(h as i32)).to_le_bytes()); // negative: top-down
    file.extend_from_slice(&1u16.to_le_bytes());
    file.extend_from_slice(&32u16.to_le_bytes());
    file.extend_from_slice(&[0; 24]); // BI_RGB, sizes, resolution, palette
    file.extend_from_slice(&bgra);
    fs::write(path, file).with_context(|| format!("could not write {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_sizes() {
        assert_eq!(parse_size("1280x720"), Ok((1280, 720)));
        assert!(parse_size("1281x720").is_err());
        assert!(parse_size("720p").is_err());
    }

    #[test]
    fn file_names() {
        assert_eq!(
            file_safe("rtsp://127.0.0.1:8554/h264-720p"),
            "rtsp___127_0_0_1_8554_h264-720p"
        );
        assert_eq!(file_safe("Front Door"), "Front_Door");
    }
}
