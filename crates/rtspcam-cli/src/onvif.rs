//! `onvif`: find cameras on the network and list their streams.

use std::process::ExitCode;
use std::time::Duration;

use anyhow::Context as _;
use clap::{Args, Subcommand};
use rtspcam_core::config::ConfigStore;
use rtspcam_onvif::{Credentials, discover, query_camera};
use url::Url;

#[derive(Subcommand)]
pub(crate) enum OnvifCommand {
    /// Find ONVIF cameras on the local network (WS-Discovery).
    Discover {
        /// How long to wait for answers.
        #[arg(long, default_value_t = 4)]
        seconds: u64,
    },
    /// Ask a camera for its streams (profiles and RTSP addresses).
    Streams(StreamsArgs),
}

#[derive(Args)]
pub(crate) struct StreamsArgs {
    /// The camera's address: a host or IP (port 80 is assumed), `host:port`, or the full
    /// `http://host/onvif/device_service` address from `discover`.
    camera: String,
    #[arg(short, long)]
    user: Option<String>,
    #[arg(short, long)]
    password: Option<String>,
    /// Use the login saved for this configured stream (so the password never appears on the
    /// command line).
    #[arg(long, value_name = "STREAM NAME", conflicts_with_all = ["user", "password"])]
    credentials_from: Option<String>,
}

pub(crate) fn run(store: &ConfigStore, cmd: OnvifCommand) -> anyhow::Result<ExitCode> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async {
        match cmd {
            OnvifCommand::Discover { seconds } => discover_cmd(seconds).await,
            OnvifCommand::Streams(args) => streams_cmd(store, args).await,
        }
    })
}

async fn discover_cmd(seconds: u64) -> anyhow::Result<ExitCode> {
    println!("Looking for ONVIF cameras for {seconds} s...");
    let found = discover(Duration::from_secs(seconds)).await?;
    if found.is_empty() {
        println!("No cameras answered. Is ONVIF on, and is the firewall letting UDP replies in?");
        return Ok(ExitCode::FAILURE);
    }
    for d in &found {
        println!(
            "{:<16} {:<28} {}",
            d.host(),
            d.title(),
            d.device_url().map_or_else(String::new, ToString::to_string)
        );
    }
    Ok(ExitCode::SUCCESS)
}

async fn streams_cmd(store: &ConfigStore, args: StreamsArgs) -> anyhow::Result<ExitCode> {
    let address = if args.camera.contains("://") {
        args.camera.clone()
    } else {
        format!("http://{}/onvif/device_service", args.camera)
    };
    let url = Url::parse(&address).with_context(|| format!("{address} is not a valid address"))?;
    let credentials = match &args.credentials_from {
        Some(name) => {
            let config = store.load()?;
            let stream = config
                .streams
                .iter()
                .find(|s| s.name.eq_ignore_ascii_case(name))
                .with_context(|| format!("no configured stream is called \"{name}\""))?;
            let (username, password) = stream
                .credentials()
                .with_context(|| format!("\"{name}\" has no usable saved login"))?;
            Some(Credentials {
                username: username.to_owned(),
                password: password.to_owned(),
            })
        }
        None => args.user.map(|username| Credentials {
            username,
            password: args.password.unwrap_or_default(),
        }),
    };
    match query_camera(&url, credentials.as_ref()).await {
        Ok(info) => {
            if let Some(title) = info.title() {
                println!("{title}");
            }
            for p in &info.profiles {
                println!("  {:<40} {}", p.label(), p.rtsp_uri);
            }
            Ok(ExitCode::SUCCESS)
        }
        Err(e) => {
            println!("{e}");
            if let Some(hint) = e.hint() {
                println!("{hint}");
            }
            Ok(ExitCode::FAILURE)
        }
    }
}
