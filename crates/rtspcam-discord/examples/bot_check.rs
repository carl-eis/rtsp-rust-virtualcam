//! Logs the bot in, prints its servers and voice channels, and optionally joins one for a
//! while, printing every status change. For checking the bot against Discord by hand.
//!
//! `RTSPCAM_DISCORD_TOKEN=... cargo run -p rtspcam-discord --example bot_check [server id channel id [seconds]] [--unmute] [--input <id>] [--listen [--output <id>]] [--follow <server id>:<user id>] [--watch <seconds>]`
//!
//! The token comes from the environment only, so it never ends up in a shell history file or
//! the repository.

#![allow(clippy::print_stdout)]

use std::time::{Duration, Instant};

use rtspcam_discord::{BotStatus, DiscordBot, ListenSettings, ServerRules, SessionOptions};

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("RTSPCAM_LOG")
                .unwrap_or_else(|_| "warn,rtspcam_discord=info,rtspcam_audio=info".into()),
        )
        .init();
    let Ok(token) = std::env::var("RTSPCAM_DISCORD_TOKEN") else {
        println!("set RTSPCAM_DISCORD_TOKEN to the bot's token");
        return;
    };
    let args: Vec<String> = std::env::args().skip(1).collect();
    let flag = |name: &str| args.iter().any(|a| a == name);
    let value = |name: &str| {
        args.iter()
            .position(|a| a == name)
            .and_then(|i| args.get(i + 1))
            .cloned()
    };
    let numbers: Vec<u64> = args
        .iter()
        .take_while(|a| !a.starts_with("--"))
        .filter_map(|a| a.parse().ok())
        .collect();

    let mut options = SessionOptions::default();
    if let Some(input) = value("--input") {
        options.input = input;
    }
    if flag("--listen") {
        options.listen = ListenSettings {
            enabled: true,
            output: value("--output").unwrap_or_else(|| "default".into()),
            volume: 1.0,
        };
    }
    // `--follow <server id>:<user id>`
    if let Some((guild, user)) = value("--follow").and_then(|f| {
        let (g, u) = f.split_once(':')?;
        Some((g.parse().ok()?, u.parse().ok()?))
    }) {
        options.servers.insert(
            guild,
            ServerRules {
                auto_join: None,
                follow: Some(user),
            },
        );
    }
    let watch_secs: Option<u64> = value("--watch").and_then(|s| s.parse().ok());
    let bot = DiscordBot::new(options);
    let mut status = bot.status();
    bot.login(token, None);

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .expect("a runtime");
    let deadline = |secs: u64| Instant::now() + Duration::from_secs(secs);
    let wait = |status: &mut tokio::sync::watch::Receiver<BotStatus>, until: Instant| {
        runtime.block_on(async {
            while let Ok(Ok(())) = tokio::time::timeout(
                until.saturating_duration_since(Instant::now()),
                status.changed(),
            )
            .await
            {
                print_status(&status.borrow_and_update());
            }
        });
    };

    // Long enough for the gateway to send the servers.
    wait(&mut status, deadline(8));
    if let Some(secs) = watch_secs {
        // Just watch (auto-join, follow) for a while.
        println!("--- watching for {secs} s");
        wait(&mut status, deadline(secs));
    }
    if let [guild, channel, rest @ ..] = numbers.as_slice() {
        let secs = rest.first().copied().unwrap_or(20);
        println!("--- joining {guild}/{channel} for {secs} s");
        bot.join(*guild, *channel);
        if flag("--unmute") {
            bot.set_muted(false);
        }
        let end = deadline(secs);
        while Instant::now() < end {
            wait(
                &mut status,
                (Instant::now() + Duration::from_secs(2)).min(end),
            );
            if let Some(meter) = bot.meter() {
                println!(
                    "    level {:.1} dB, gate open {}",
                    meter.level.peak_db, meter.gate_open
                );
            }
        }
        bot.leave();
        wait(&mut status, deadline(2));
    }
    bot.shutdown(Duration::from_secs(3));
    println!("--- logged out");
}

fn print_status(status: &BotStatus) {
    match status {
        BotStatus::Online(online) => {
            println!("online as {} (muted {})", online.user, online.muted);
            if let Some(url) = &online.invite_url {
                println!("  invite: {url}");
            }
            for server in &online.servers {
                println!("  server {} [{}]", server.name, server.id);
                for c in &server.channels {
                    println!(
                        "    {} [{}]{}{}",
                        c.name,
                        c.id,
                        if c.stage { " stage" } else { "" },
                        if c.can_speak { "" } else { " (can't speak)" }
                    );
                }
                for m in &server.members {
                    println!("    - {} [{}] in {}", m.name, m.id, m.channel_id);
                }
            }
            if let Some(voice) = &online.voice {
                println!(
                    "  voice: {}/{} {:?} lag {} ms dropped {} stage {:?} speakers {}",
                    voice.guild_id,
                    voice.channel_id,
                    voice.state,
                    voice.lag_ms,
                    voice.dropped,
                    voice.stage,
                    voice.speakers
                );
            }
            println!("  input: {:?}", online.input);
            if online.listening {
                println!("  output: {:?}", online.output);
            }
            if let Some(notice) = &online.notice {
                println!("  notice: {} / {}", notice.text, notice.hint);
            }
        }
        other => println!("{other:?}"),
    }
}
