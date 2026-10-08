//! `rtspcam-cli`: developer tool.
//!
//! - `config`: inspect and edit the config file.
//! - `probe`, `view`: connect to RTSP streams and decode them without the app.
//!
//! Phase 3 adds `vcam` commands.

#![allow(clippy::print_stdout)]

mod rtsp;

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::Context as _;
use clap::{Parser, Subcommand};
use rtspcam_core::config::ConfigStore;
use rtspcam_core::logging::{self, LogOptions};
use rtspcam_core::{LogLevel, Secret, StreamConfig, paths};
use serde_json::Value;

#[derive(Parser)]
#[command(version, about = "RTSP Cam developer tool")]
struct Cli {
    /// Use this config file instead of %APPDATA%\RtspCam\config.json.
    #[arg(long, global = true, value_name = "FILE")]
    config: Option<PathBuf>,

    /// Log to stderr at debug level.
    #[arg(short, long, global = true)]
    verbose: bool,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Inspect or edit the config file.
    #[command(subcommand)]
    Config(ConfigCommand),
    /// Connect to a stream, show what it offers and decode one picture.
    Probe(rtsp::ProbeArgs),
    /// Run streams through the full pipeline and print their status every second.
    View(rtsp::ViewArgs),
}

#[derive(Subcommand)]
enum ConfigCommand {
    /// Print the config file path.
    Path,
    /// Print the config as JSON, with passwords redacted.
    Show,
    /// Check the config and list any problems. Exits with status 1 if there are any.
    Validate,
    /// Add the streams published by tools/test-rtsp (mediamtx). Existing names are skipped.
    AddTestStreams {
        /// Host running mediamtx.
        #[arg(long, default_value = "127.0.0.1")]
        host: String,
        /// mediamtx RTSP port.
        #[arg(long, default_value_t = 8554)]
        port: u16,
    },
}

/// Streams published by `tools/test-rtsp/mediamtx.yml`: (name, path, needs credentials).
const TEST_STREAMS: &[(&str, &str, bool)] = &[
    ("Test H.264 720p", "/h264-720p", false),
    ("Test H.264 1080p", "/h264-1080p", false),
    ("Test H.265 720p", "/h265-720p", false),
    ("Test MJPEG 720p", "/mjpeg-720p", false),
    ("Test Secure", "/secure", true),
];
const TEST_USER: &str = "rtspcam";
const TEST_PASSWORD: &str = "rtspcam-test";

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(cli) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("error: {e:#}");
            ExitCode::FAILURE
        }
    }
}

fn run(cli: Cli) -> anyhow::Result<ExitCode> {
    let mut log_opts = LogOptions::new(paths::log_dir()?, "rtspcam-cli");
    log_opts.stderr = true;
    log_opts.level = if cli.verbose {
        LogLevel::Debug
    } else {
        LogLevel::Warn
    };
    let _log = logging::init(&log_opts)?;

    let store = match cli.config {
        Some(path) => ConfigStore::new(path),
        None => ConfigStore::open_default()?,
    };

    match cli.command {
        Command::Config(cmd) => config_command(&store, cmd),
        Command::Probe(args) => rtsp::probe(&store, args),
        Command::View(args) => rtsp::view(&store, args),
    }
}

fn config_command(store: &ConfigStore, cmd: ConfigCommand) -> anyhow::Result<ExitCode> {
    match cmd {
        ConfigCommand::Path => println!("{}", store.path().display()),
        ConfigCommand::Show => {
            let config = store.load()?;
            let mut json = serde_json::to_value(&config)?;
            redact_passwords(&mut json);
            println!("{}", serde_json::to_string_pretty(&json)?);
        }
        ConfigCommand::Validate => {
            let config = store.load()?;
            let issues = config.validate();
            for issue in &issues {
                let name = config.stream(issue.stream).map_or("?", |s| s.name.as_str());
                println!(
                    "{name} ({}): {:?}: {}",
                    issue.stream, issue.field, issue.problem
                );
            }
            if !issues.is_empty() {
                return Ok(ExitCode::FAILURE);
            }
            println!(
                "{}: OK ({} streams)",
                store.path().display(),
                config.streams.len()
            );
        }
        ConfigCommand::AddTestStreams { host, port } => {
            let mut config = store.load()?;
            for &(name, path, secure) in TEST_STREAMS {
                if config
                    .streams
                    .iter()
                    .any(|s| s.name.eq_ignore_ascii_case(name))
                {
                    println!("skip  {name} (already exists)");
                    continue;
                }
                let mut stream = StreamConfig::new(name, host.as_str());
                stream.port = port;
                stream.path = path.into();
                if secure {
                    stream.username = TEST_USER.into();
                    stream.password = Some(Secret::new(TEST_PASSWORD));
                }
                println!("add   {name}  {}", stream.url());
                config.streams.push(stream);
            }
            store.save(&config).context("could not save the config")?;
        }
    }
    Ok(ExitCode::SUCCESS)
}

fn redact_passwords(json: &mut Value) {
    if let Some(streams) = json.get_mut("streams").and_then(Value::as_array_mut) {
        for stream in streams {
            if let Some(pw) = stream.get_mut("password") {
                *pw = "<redacted>".into();
            }
        }
    }
}
