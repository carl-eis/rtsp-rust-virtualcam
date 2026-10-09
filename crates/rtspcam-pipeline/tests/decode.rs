//! The OpenH264 decoder against H.264 produced by the OpenH264 encoder. (The platform decoders
//! are tested in `rtspcam-platform`.)

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
