//! Lists the audio devices, then records two seconds from one and prints its level.
//!
//! `cargo run -p rtspcam-audio --example audio_check [device id] [--tone]`
//!
//! `--tone` plays a quiet 440 Hz tone on the default output meanwhile (to check playback, and
//! loopback recording of an output device: WASAPI delivers nothing while the output is silent).

#![allow(clippy::print_stdout)]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use rtspcam_audio::{
    Block, BlockQueue, Capture, DEFAULT_DEVICE, Level, Playback, SAMPLE_RATE, devices,
    loopback_supported,
};

fn main() {
    println!("loopback supported: {}", loopback_supported());
    match devices() {
        Ok(list) => {
            for d in list {
                let star = if d.is_default { "*" } else { " " };
                println!("{star} {:?}  {}  [{}]", d.kind, d.name, d.id);
            }
        }
        Err(e) => println!("could not list devices: {e}"),
    }

    let args: Vec<String> = std::env::args().skip(1).collect();
    let tone = args.iter().any(|a| a == "--tone");
    let id = args
        .iter()
        .find(|a| !a.starts_with("--"))
        .cloned()
        .unwrap_or_else(|| DEFAULT_DEVICE.into());
    let _playback = tone.then(play_tone);
    let levels = Arc::new(Mutex::new(Vec::new()));
    let sink = levels.clone();
    let capture = Capture::open(
        &id,
        move |block| sink.lock().unwrap().push(Level::of(&block)),
        |e| println!("capture failed: {e}"),
    );
    match capture {
        Ok(capture) => {
            println!("recording from {:?}", capture.info());
            std::thread::sleep(Duration::from_secs(2));
            drop(capture);
            let levels = levels.lock().unwrap();
            let peak = levels.iter().map(|l| l.peak_db).fold(f32::MIN, f32::max);
            println!(
                "{} blocks of 20 ms; loudest peak {peak:.1} dBFS",
                levels.len()
            );
        }
        Err(e) => println!("could not open {id}: {e}"),
    }
}

/// Two and a half seconds of a 440 Hz tone at -20 dBFS on the default output.
fn play_tone() -> Option<Playback> {
    let queue = Arc::new(BlockQueue::new(Duration::from_secs(3)));
    let mut phase = 0.0_f32;
    for _ in 0..125 {
        let mut block = Block::silence();
        for frame in block.as_chunks_mut::<2>().0 {
            let s = 0.1 * phase.sin();
            frame.copy_from_slice(&[s, s]);
            phase += std::f32::consts::TAU * 440.0 / SAMPLE_RATE as f32;
        }
        queue.push(block);
    }
    match Playback::open(DEFAULT_DEVICE, queue, |e| println!("playback failed: {e}")) {
        Ok(p) => {
            println!("playing a tone on {}", p.device_name());
            Some(p)
        }
        Err(e) => {
            println!("could not play: {e}");
            None
        }
    }
}
