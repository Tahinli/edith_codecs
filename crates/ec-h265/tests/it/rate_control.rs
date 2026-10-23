//! Bitrate control of the sequence encoder: a clip coded at a target lands within
//! 5% of that target, and the output does not depend on the worker count.
//!
//! The clip is a textured pattern that moves a few samples per picture, so the
//! pictures differ in cost and the model has to follow them. The 5% bound is the
//! same one the 60 s measurement is held to (`scripts/hevc-rc.sh`).

use ec_core::frame::{PixelFormat, Plane, VideoFrame};
use ec_h265::encoder::{EncodedPicture, Encoder, EncoderConfig, RateControl};

const W: usize = 320;
const H: usize = 240;
const FPS: f64 = 30.0;
const FRAMES: usize = 150;
const KEYINT: usize = 30;

fn moving_clip(frames: usize) -> Vec<VideoFrame> {
    let (cw, ch) = (W / 2, H / 2);
    (0..frames)
        .map(|t| {
            let mut y = vec![0u8; W * H];
            for r in 0..H {
                for c in 0..W {
                    let (x, yy) = (c + 3 * t, r + t);
                    let mut v = (x * 7 + yy * 5) as u32;
                    v ^= (x as u32).wrapping_mul(0x9e37) ^ (yy as u32).wrapping_mul(0x85eb);
                    y[r * W + c] = (v.wrapping_mul(2_654_435_761) >> 24) as u8;
                }
            }
            let chroma = |scale: usize| -> Vec<u8> {
                (0..cw * ch)
                    .map(|i| {
                        let (c, r) = (i % cw, i / cw);
                        ((c * scale + r * 3 + t * 2) % 251) as u8
                    })
                    .collect()
            };
            VideoFrame::try_new(
                PixelFormat::I420,
                W as u32,
                H as u32,
                vec![
                    Plane::new(y, W),
                    Plane::new(chroma(5), cw),
                    Plane::new(chroma(9), cw),
                ],
            )
            .expect("I420 planes at the stated size")
        })
        .collect()
}

/// Encode the clip at `kbps` with `threads` workers; returns the stream bytes.
fn encode_at(frames: &[VideoFrame], kbps: u32, threads: usize) -> Vec<u8> {
    let mut cfg = EncoderConfig::new(W as u32, H as u32);
    cfg.rate_control = RateControl::Bitrate { kbps, fps: FPS };
    cfg.keyint = KEYINT;
    cfg.threads = threads;
    let enc = Encoder::new(cfg).expect("encoder");
    let coded = enc.encode_sequence(frames).expect("encode");
    coded.iter().flat_map(|p| p.au.iter().copied()).collect()
}

#[test]
fn bitrate_lands_within_five_percent_of_each_target() {
    let frames = moving_clip(FRAMES);
    for kbps in [1000u32, 2500, 5000] {
        let stream = encode_at(&frames, kbps, 1);
        let seconds = FRAMES as f64 / FPS;
        let achieved = stream.len() as f64 * 8.0 / seconds / 1000.0;
        let error = (achieved - f64::from(kbps)) / f64::from(kbps);
        eprintln!(
            "target {kbps} kbps: achieved {achieved:.1} kbps ({:+.2}%)",
            error * 100.0
        );
        assert!(
            error.abs() <= 0.05,
            "target {kbps} kbps: achieved {achieved:.1} kbps is {:+.2}% off",
            error * 100.0
        );
    }
}

/// A flat picture at `luma` (chroma at 128): nothing in it costs more than a skipped block
/// at any QP, so it is the case where the rate cannot be spent.
fn flat_clip(frames: usize, luma: u8) -> Vec<VideoFrame> {
    let (cw, ch) = (W / 2, H / 2);
    (0..frames)
        .map(|_| {
            VideoFrame::try_new(
                PixelFormat::I420,
                W as u32,
                H as u32,
                vec![
                    Plane::new(vec![luma; W * H], W),
                    Plane::new(vec![128; cw * ch], cw),
                    Plane::new(vec![128; cw * ch], cw),
                ],
            )
            .expect("I420 planes at the stated size")
        })
        .collect()
}

/// A flat clip at 2000 kbps cannot spend its budget even at QP 0, so the rate model must
/// walk the quantiser down to the HEVC floor and hold it there, rather than stall high.
/// The floor is the codec's, not a rate floor of this encoder: whatever bits remain are the
/// content's own cost at QP 0.
#[test]
fn flat_near_black_descends_to_the_qp_floor() {
    let frames = flat_clip(60, 16);
    let pics: Vec<EncodedPicture> = encode_pictures(&frames, 2000);
    // The first predicted pictures walk the quantiser down from the IDR's QP (at most four
    // steps each); from picture 10 on, every predicted picture must sit at QP 0.
    let tail: Vec<(usize, i32)> = pics
        .iter()
        .enumerate()
        .filter(|(i, _)| i % KEYINT != 0 && *i >= 10)
        .map(|(i, p)| (i, p.qp))
        .collect();
    assert!(!tail.is_empty());
    let stalled: Vec<&(usize, i32)> = tail.iter().filter(|(_, qp)| *qp != 0).collect();
    assert!(
        stalled.is_empty(),
        "QP did not reach the floor: {stalled:?}"
    );
}

/// Encode `frames` at `kbps` and return the coded pictures, so the test can see each QP.
fn encode_pictures(frames: &[VideoFrame], kbps: u32) -> Vec<EncodedPicture> {
    let mut cfg = EncoderConfig::new(W as u32, H as u32);
    cfg.rate_control = RateControl::Bitrate { kbps, fps: FPS };
    cfg.keyint = KEYINT;
    cfg.threads = 1;
    Encoder::new(cfg)
        .expect("encoder")
        .encode_sequence(frames)
        .expect("encode")
}

#[test]
fn bitrate_output_is_identical_at_any_thread_count() {
    let frames = moving_clip(FRAMES);
    let one = encode_at(&frames, 2500, 1);
    for threads in [2usize, 4] {
        assert!(
            encode_at(&frames, 2500, threads) == one,
            "stream differs at {threads} threads"
        );
    }
}

/// Near-black with temporal noise at 720p: luma 16 plus a deterministic PRNG offset of -2..=2
/// that differs every picture. Cost against QP is a cliff here (a few hundred bytes per
/// picture above QP ~17, tens of kilobytes a few steps below), so the `2^(QP/6)` model alone
/// dives into the cliff after a run of cheap pictures and the stream overshoots. The cliff
/// only shows at this size, which is why this clip is not 320x240.
fn noisy_black_clip(frames: usize, w: usize, h: usize) -> Vec<VideoFrame> {
    let (cw, ch) = (w / 2, h / 2);
    let mut state = 0x2545_f491_4f6c_dd1du64;
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        (state >> 33) as u32
    };
    (0..frames)
        .map(|_| {
            let y: Vec<u8> = (0..w * h).map(|_| (14 + next() % 5) as u8).collect();
            VideoFrame::try_new(
                PixelFormat::I420,
                w as u32,
                h as u32,
                vec![
                    Plane::new(y, w),
                    Plane::new(vec![128; cw * ch], cw),
                    Plane::new(vec![128; cw * ch], cw),
                ],
            )
            .expect("I420 planes at the stated size")
        })
        .collect()
}

#[test]
fn noisy_near_black_lands_within_five_percent() {
    let (w, h, frames) = (1280usize, 720usize, noisy_black_clip(90, 1280, 720));
    for kbps in [500u32, 2000] {
        let mut cfg = EncoderConfig::new(w as u32, h as u32);
        cfg.rate_control = RateControl::Bitrate { kbps, fps: FPS };
        cfg.keyint = KEYINT;
        cfg.threads = 1;
        let pics = Encoder::new(cfg)
            .expect("encoder")
            .encode_sequence(&frames)
            .expect("encode");
        let bytes: usize = pics.iter().map(|p| p.au.len()).sum();
        let achieved = bytes as f64 * 8.0 / (frames.len() as f64 / FPS) / 1000.0;
        let error = (achieved - f64::from(kbps)) / f64::from(kbps);
        eprintln!("target {kbps}: {achieved:.1} kbps ({:+.2}%)", error * 100.0);
        assert!(
            error.abs() <= 0.05,
            "target {kbps} kbps: achieved {achieved:.1} kbps is {:+.2}% off",
            error * 100.0
        );
    }
}
