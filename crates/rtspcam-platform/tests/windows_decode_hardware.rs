//! Hardware (Direct3D 11) decoding through Media Foundation. A test binary of its own, because
//! the hardware switch is process-wide and the software tests must stay in software.
//!
//! Every test skips (passes with a message) on a computer without a graphics card that decodes
//! H.264, such as CI runners and most VMs. Run them on a machine with one before a release:
//! `cargo test -p rtspcam-platform --test windows_decode_hardware -- --nocapture`.
#![cfg(windows)]

use std::time::{Duration, Instant};

use openh264::encoder::Encoder;
use openh264::formats::YUVBuffer;
use rtspcam_pipeline::decode::{
    self, DecoderChoice, HardwareDecoding, create_decoder, create_decoder_with,
};
use rtspcam_pipeline::{EncodedFrame, Frame, VideoCodec};

const W: usize = 320;
const H: usize = 240;

/// I420 picture: left half dark, right half bright, neutral chroma.
fn picture() -> Vec<u8> {
    let mut yuv = vec![128u8; W * H * 3 / 2];
    for row in 0..H {
        for col in 0..W {
            yuv[row * W + col] = if col < W / 2 { 40 } else { 200 };
        }
    }
    yuv
}

fn stream(frames: usize) -> Vec<EncodedFrame> {
    let mut encoder = Encoder::new().unwrap();
    (0..frames)
        .map(|i| EncodedFrame {
            codec: VideoCodec::H264,
            keyframe: i == 0,
            pts: Duration::from_millis(33 * i as u64),
            received: Instant::now(),
            loss: 0,
            data: encoder
                .encode(&YUVBuffer::from_vec(picture(), W, H))
                .unwrap()
                .to_vec(),
        })
        .collect()
}

/// Turns hardware decoding on; `false` (skip) if this computer has none for H.264.
fn hardware() -> bool {
    rtspcam_platform::install();
    decode::set_use_hardware(true);
    let until = Instant::now() + Duration::from_secs(5);
    loop {
        match decode::hardware_decoding() {
            HardwareDecoding::Checking if Instant::now() < until => {
                std::thread::sleep(Duration::from_millis(50));
            }
            HardwareDecoding::Available { device, codecs }
                if codecs.contains(&VideoCodec::H264) =>
            {
                eprintln!("decoding on {device}");
                return true;
            }
            other => {
                eprintln!(
                    "skipped: no hardware H.264 decoding here ({})",
                    other.describe()
                );
                return false;
            }
        }
    }
}

fn check_split(f: &Frame) {
    let (w, h) = (f.width() as usize, f.height() as usize);
    let row = h / 2 * w;
    let y = f.y();
    assert!(
        y[row + w / 16].abs_diff(40) < 14,
        "dark side {}",
        y[row + w / 16]
    );
    assert!(
        y[row + w - w / 16].abs_diff(200) < 14,
        "bright side {}",
        y[row + w - w / 16]
    );
    assert!(f.uv().iter().all(|&c| c.abs_diff(128) < 14));
}

fn decode_all(decoder: &mut dyn decode::Decoder, frames: &[EncodedFrame]) -> Vec<Frame> {
    let mut out = Vec::new();
    for f in frames {
        decoder.decode(f, &mut out).unwrap();
    }
    out
}

#[test]
fn decodes_on_the_gpu_at_full_size() {
    if !hardware() {
        return;
    }
    let mut decoder = create_decoder(
        VideoCodec::H264,
        Some((W as u32, H as u32)),
        DecoderChoice::Platform,
    )
    .unwrap();
    let frames = decode_all(decoder.as_mut(), &stream(30));
    assert!(frames.len() >= 25, "only {} pictures", frames.len());
    assert!(decoder.name().ends_with(", D3D11)"), "{}", decoder.name());
    for f in &frames {
        assert_eq!((f.width(), f.height()), (W as u32, H as u32));
        check_split(f);
    }
    assert!(frames.windows(2).all(|w| w[0].pts < w[1].pts));
}

#[test]
fn shrinks_pictures_on_the_gpu() {
    if !hardware() {
        return;
    }
    let mut decoder = create_decoder(
        VideoCodec::H264,
        Some((W as u32, H as u32)),
        DecoderChoice::Platform,
    )
    .unwrap();
    decoder.limit_size(Some((160, 90)));
    let frames = decode_all(decoder.as_mut(), &stream(10));
    assert!(!frames.is_empty());
    for f in &frames {
        // Covers 160 wide; keeps the 4:3 shape.
        assert_eq!((f.width(), f.height()), (160, 120));
        assert_eq!(
            f.source_size,
            (W as u32, H as u32),
            "the colours follow the camera"
        );
        check_split(f);
    }
    // Back to full size.
    decoder.limit_size(None);
    let frames = decode_all(decoder.as_mut(), &stream(5));
    assert!(frames.iter().all(|f| f.width() == W as u32));
}

#[test]
fn software_can_still_be_asked_for() {
    if !hardware() {
        return;
    }
    let mut decoder = create_decoder_with(
        VideoCodec::H264,
        Some((W as u32, H as u32)),
        DecoderChoice::Platform,
        false,
    )
    .unwrap();
    let frames = decode_all(decoder.as_mut(), &stream(10));
    assert!(!frames.is_empty());
    assert!(!decoder.name().contains("D3D11"), "{}", decoder.name());
    // A software decoder ignores the size limit.
    decoder.limit_size(Some((160, 90)));
    let frames = decode_all(decoder.as_mut(), &stream(5));
    assert!(frames.iter().all(|f| f.width() == W as u32));
}
