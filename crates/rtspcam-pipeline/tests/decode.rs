//! Decoders against H.264 produced by the OpenH264 encoder, and an MJPEG fixture.

// The H.264 input is made with the OpenH264 encoder.
#![cfg(feature = "openh264")]

use std::time::{Duration, Instant};

use openh264::encoder::Encoder;
use openh264::formats::YUVBuffer;
use rtspcam_pipeline::decode::{DecoderChoice, create_decoder};
use rtspcam_pipeline::{EncodedFrame, Frame, VideoCodec};

const W: usize = 320;
const H: usize = 240;

/// I420 picture: left half dark, right half bright, neutral chroma. `shift` moves the edge.
fn picture(shift: usize) -> Vec<u8> {
    let mut yuv = vec![128u8; W * H * 3 / 2];
    for row in 0..H {
        for col in 0..W {
            yuv[row * W + col] = if col < W / 2 + shift { 40 } else { 200 };
        }
    }
    yuv
}

fn encode_h264(frames: usize) -> Vec<Vec<u8>> {
    let mut encoder = Encoder::new().unwrap();
    (0..frames)
        .map(|i| {
            let yuv = YUVBuffer::from_vec(picture(i % 4), W, H);
            encoder.encode(&yuv).unwrap().to_vec()
        })
        .collect()
}

fn encoded(codec: VideoCodec, data: Vec<u8>, i: usize) -> EncodedFrame {
    EncodedFrame {
        codec,
        keyframe: i == 0,
        pts: Duration::from_millis(33 * i as u64),
        received: Instant::now(),
        loss: 0,
        data,
    }
}

fn check_picture(f: &Frame) {
    assert_eq!((f.width(), f.height()), (W as u32, H as u32));
    let y = f.y();
    let row = H / 2 * W;
    // Allow for compression error and the moving edge.
    assert!(y[row + 10].abs_diff(40) < 12, "dark side {}", y[row + 10]);
    assert!(
        y[row + W - 10].abs_diff(200) < 12,
        "bright side {}",
        y[row + W - 10]
    );
    assert!(f.uv().iter().all(|&c| c.abs_diff(128) < 12));
}

fn decode_all(choice: DecoderChoice, codec: VideoCodec, inputs: Vec<Vec<u8>>) -> Vec<Frame> {
    let mut decoder = create_decoder(codec, Some((W as u32, H as u32)), choice).unwrap();
    let mut out = Vec::new();
    for (i, data) in inputs.into_iter().enumerate() {
        decoder.decode(&encoded(codec, data, i), &mut out).unwrap();
    }
    out
}

#[cfg(windows)]
#[test]
fn media_foundation_h264() {
    let frames = decode_all(
        DecoderChoice::MediaFoundation,
        VideoCodec::H264,
        encode_h264(30),
    );
    // Low-latency mode should hand back nearly every frame.
    assert!(frames.len() >= 25, "only {} pictures", frames.len());
    frames.iter().for_each(check_picture);
    assert!(frames.windows(2).all(|w| w[0].pts < w[1].pts));
}

#[test]
fn openh264_h264() {
    let frames = decode_all(DecoderChoice::OpenH264, VideoCodec::H264, encode_h264(30));
    assert_eq!(frames.len(), 30);
    frames.iter().for_each(check_picture);
}

#[test]
fn openh264_refuses_other_codecs() {
    assert!(create_decoder(VideoCodec::H265, None, DecoderChoice::OpenH264).is_err());
}

#[cfg(windows)]
#[test]
fn media_foundation_mjpeg() {
    // 320x240 JPEG of the same split picture, made with:
    // ffmpeg -f lavfi -i "color=c=0x282828:s=160x240,format=yuvj420p[l];color=c=0xc8c8c8:s=160x240,format=yuvj420p[r];[l][r]hstack" -frames:v 1 -q:v 2 split.jpg
    let jpeg = include_bytes!("fixtures/split-320x240.jpg").to_vec();
    let mut decoder = create_decoder(
        VideoCodec::Mjpeg,
        Some((W as u32, H as u32)),
        DecoderChoice::MediaFoundation,
    )
    .unwrap();
    let mut out = Vec::new();
    for i in 0..3 {
        decoder
            .decode(&encoded(VideoCodec::Mjpeg, jpeg.clone(), i), &mut out)
            .unwrap();
    }
    assert!(!out.is_empty());
    let f = &out[0];
    assert_eq!((f.width(), f.height()), (W as u32, H as u32));
    let row = H / 2 * W;
    // JPEG is full range: 0x28 = 40, 0xc8 = 200 (the decoder may or may not convert to video
    // range, so only check which side is darker).
    assert!(f.y()[row + 10] + 80 < f.y()[row + W - 10]);
}
